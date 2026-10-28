import { test } from "node:test"
import assert from "node:assert/strict"
import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import http from "node:http"
import { createCapture, probe } from "./capture.mjs"

async function fixture(t, settings = {}) {
  const work = await mkdtemp(join(tmpdir(), "proofstorm-capture-"))
  const server = http.createServer((_, response) => response.end("ok"))
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve))
  const url = `http://127.0.0.1:${server.address().port}`
  const capture = createCapture({ work, ...settings })
  t.after(async () => {
    await capture.close()
    await new Promise(resolve => server.close(resolve))
    await rm(work, { recursive: true, force: true })
  })
  const rows = async () => {
    await capture.flush()
    return (await readFile(join(work, "transport.private.jsonl"), "utf8")).trim().split("\n").map(JSON.parse)
  }
  return { work, url, capture, rows, server }
}

test("HTTP 200 SSE error retains correlation, timing and identical response bytes without credential headers", async t => {
  const { url, capture, rows, work } = await fixture(t)
  const request = new Request(url + "/v1/chat?key=hidden-query", { method: "POST",
    headers: { authorization: "Bearer secret-fixture-key", "x-api-key": "custom-secret-key", "content-type": "application/json" },
    body: JSON.stringify({ model: "fixture", password: "hidden-password", messages: [{ content: "secret-fixture-key" }] }) })
  await capture.request({ request, sessionID: "session", kind: "primary", model: { id: "fixture" } })
  assert.equal(JSON.parse(await request.text()).password, "hidden-password")
  const payload = 'data: {"id":"provider-response-1","error":{"code":"provider_unavailable","status":503,"message":"secret-fixture-key"}}\n\ndata: [DONE]\n\n'
  const event = { request, response: new Response(payload, { headers: {
    "content-type": "text/event-stream", "x-request-id": "provider-http-1", "set-cookie": "hidden-cookie", "x-secret": "hidden-custom",
  } }) }
  capture.response(event)
  assert.equal(await event.response.text(), payload)
  await capture.close()
  const log = await rows()
  const req = log.find(r => r.type === "request")
  const response = log.find(r => r.type === "response_headers")
  const error = log.find(r => r.type === "provider_error_payload")
  assert.equal(req.request_id, response.request_id)
  assert.equal(error.request_id, req.request_id)
  assert.equal(response.headers["x-request-id"], "provider-http-1")
  assert.equal(response.status, 200)
  assert.equal(error.error.status, 503)
  assert.equal(error.response_id, "provider-response-1")
  assert.ok(log.find(r => r.type === "first_chunk").elapsed_ms >= 0)
  assert.equal(log.find(r => r.type === "stream_end").complete, true)
  assert.equal(log.filter(r => r.type === "response_identity").length, 1)
  const text = JSON.stringify(log)
  for (const secret of ["secret-fixture-key", "custom-secret-key", "hidden-query", "hidden-password", "hidden-cookie", "hidden-custom"]) assert.ok(!text.includes(secret), secret)
  const status = JSON.parse(await readFile(join(work, "transport-capture-status.json")))
  assert.equal(status.write_errors, 0)
  assert.deepEqual(status.pending_requests, [])
  assert.equal(status.drain_complete, true)
})

test("reset, consumer cancellation, abort signal and retry remain distinct observations", async t => {
  const { url, capture, rows } = await fixture(t)
  const abort = new AbortController()
  const request = new Request(url, { signal: abort.signal })
  await capture.request({ request })
  let n = 0
  const reset = Object.assign(Error("private arbitrary message"), { code: "ECONNRESET" })
  const event = { request, response: new Response(new ReadableStream({ pull(c) {
    if (n++ === 0) c.enqueue(new TextEncoder().encode("partial")); else c.error(reset)
  } })) }
  capture.response(event)
  abort.abort(Object.assign(Error(), { code: "LOCAL_TIMEOUT" }))
  await assert.rejects(event.response.text(), { code: "ECONNRESET" })
  const next = new Request(url)
  await capture.request({ request: next })
  let cancelled = false
  const other = { request: next, response: new Response(new ReadableStream({ cancel() { cancelled = true } })) }
  capture.response(other)
  await other.response.body.cancel("local timeout")
  assert.equal(cancelled, true)
  const retry = { attempt: 2, decision: { type: "retry", delay: 500 }, error: { type: "transport", code: "ECONNRESET" } }
  const before = JSON.stringify(retry)
  await capture.retry(retry)
  assert.equal(JSON.stringify(retry), before)
  const log = await rows()
  assert.equal(log.find(r => r.type === "stream_error").error.code, "ECONNRESET")
  assert.equal(log.find(r => r.type === "request_signal_abort").reason.code, "LOCAL_TIMEOUT")
  assert.equal(log.filter(r => r.type === "consumer_cancel").length, 1)
  assert.equal(log.find(r => r.type === "client_retry_decision").attempt, 2)
  assert.ok(!JSON.stringify(log).includes("private arbitrary message"))
})

test("bounded request clone never blocks the original and oversized bodies are explicit", async t => {
  const { url, capture, rows } = await fixture(t, { body_limit_bytes: 16 })
  const body = JSON.stringify({ payload: "a".repeat(1000) })
  const request = new Request(url, { method: "POST", body })
  await capture.request({ request })
  assert.equal(await request.text(), body)
  const event = { request, response: new Response(body) }
  capture.response(event)
  assert.equal(await event.response.text(), body)
  const log = await rows()
  const req = log.find(r => r.type === "request_body")
  assert.equal(req.captured_bytes, 16)
  assert.equal(req.complete, false)
  assert.equal(req.body, null)
  const res = log.find(r => r.type === "stream_end")
  assert.equal(res.bytes_read, Buffer.byteLength(body))
  assert.equal(res.captured_bytes, 16)
  assert.equal(res.body_truncated, true)
})

test("log exhaustion is visible and does not alter delivery", async t => {
  const { url, work, capture } = await fixture(t, { log_limit_bytes: 1 })
  const request = new Request(url)
  await capture.request({ request })
  const event = { request, response: new Response("still delivered") }
  capture.response(event)
  assert.equal(await event.response.text(), "still delivered")
  await capture.close()
  const status = JSON.parse(await readFile(join(work, "transport-capture-status.json")))
  assert.ok(status.dropped_records > 0)
})

test("capture filesystem failure is fail-open", async t => {
  const { url, capture, work } = await fixture(t)
  await capture.flush()
  await rm(work, { recursive: true })
  const request = new Request(url)
  await capture.request({ request })
  const event = { request, response: new Response("still delivered") }
  capture.response(event)
  assert.equal(await event.response.text(), "still delivered")
  await capture.close()
})

test("missing Request identity is a capture gap, never guessed correlation", async t => {
  const { capture, rows, url } = await fixture(t)
  const event = { request: new Request(url), response: new Response("body") }
  capture.response(event)
  assert.equal(await event.response.text(), "body")
  assert.equal((await rows()).filter(r => r.type === "correlation_gap").length, 1)
})

test("route probes report DNS and TCP independently without a root-cause verdict", async t => {
  const { url, server } = await fixture(t)
  const success = await probe(url + "/sensitive?secret=hidden")
  assert.equal(success.origin, url)
  assert.equal(success.dns.ok, true)
  assert.equal(success.routes[0].tcp.ok, true)
  const protocolFailure = await probe(url.replace("http:", "https:"))
  assert.equal(protocolFailure.routes[0].tcp.ok, true)
  assert.equal(protocolFailure.routes[0].tls.ok, false)
  await new Promise(resolve => server.close(resolve))
  const failed = await probe(url)
  assert.equal(failed.dns.ok, true)
  assert.equal(failed.routes[0].tcp.ok, false)
  assert.equal(failed.routes[0].tcp.error.code, "ECONNREFUSED")
  assert.equal(failed.root_cause, undefined)
})

test("SSE parser discards an oversized frame but still captures later real errors", async t => {
  const { capture, rows, url } = await fixture(t)
  const request = new Request(url)
  await capture.request({ request })
  const chunks = ["data: " + "x".repeat(66000), 'data: {"error":{"code":"not-a-real-frame"}}\n\n',
    'data: {"id":"real-id","error":{"code":503}}\n\n']
  const event = { request, response: new Response(new ReadableStream({ start(c) {
    for (const chunk of chunks) c.enqueue(new TextEncoder().encode(chunk))
    c.close()
  } }), { headers: { "content-type": "text/event-stream" } }) }
  capture.response(event)
  assert.equal(await event.response.text(), chunks.join(""))
  const log = await rows()
  assert.deepEqual(log.filter(r => r.type === "provider_error_payload").map(r => r.error.code), [503])
  assert.equal(log.find(r => r.type === "stream_end").sse_overflow_frames, 1)
})
