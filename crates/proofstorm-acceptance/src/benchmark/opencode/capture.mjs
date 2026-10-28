// HTTP observations are private evidence, never a replacement for the model stream.
import { appendFile, writeFile } from "node:fs/promises"
import { createHash, randomUUID } from "node:crypto"
import { lookup } from "node:dns/promises"
import net from "node:net"
import tls from "node:tls"

const allowed = /^(accept|content-type|content-length|date|server|retry-after|(?:x-)?(?:[a-z0-9]+-)?(?:request|correlation|trace)-id|x-amzn-requestid|x-amzn-trace-id|cf-ray|traceparent|x-cloud-trace-context|x-envoy-upstream-service-time|x-ratelimit-[a-z-]+|ratelimit-[a-z-]+)$/i
const credential = /authorization|cookie|api[-_]?key|password|secret|access[-_]?token|refresh[-_]?token/i
export function headers(value) {
  return Object.fromEntries([...value].filter(([key]) => allowed.test(key) && !credential.test(key)).map(([key, value]) => [key, value.slice(0, 2048)]))
}
export function origin(value) {
  const url = new URL(value)
  if (!["http:", "https:"].includes(url.protocol)) throw Error("Only HTTP(S) probe origins are supported")
  return url.origin // No userinfo, path, fragment or query credentials.
}
function safeError(error) {
  return { name: String(error?.name ?? "Error").slice(0, 80), code: String(error?.code ?? error?.cause?.code ?? "").slice(0, 80) }
}
function redact(text, secrets) {
  for (const secret of secrets) if (secret.length >= 4) text = text.replaceAll(secret, "[REDACTED]")
  try {
    return JSON.stringify(JSON.parse(text), (key, value) => credential.test(key) ? "[REDACTED]" : value)
  } catch {
    // Partial/non-JSON bodies may contain credentials. Retain their hash/length,
    // not an unstructured prefix which could defeat structured redaction.
    return null
  }
}
function deadline(promise, ms) {
  let timer
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(Object.assign(Error("observation timeout"), { code: "ETIMEDOUT" })), ms)
  })]).finally(() => clearTimeout(timer))
}
function connect(address, port, servername, secure) {
  return new Promise(resolve => {
    const start = performance.now()
    const socket = secure ? tls.connect({ host: address, port, servername, rejectUnauthorized: true }) : net.connect({ host: address, port })
    let done = false
    function finish(error) {
      if (done) return
      done = true
      const result = { ok: !error, elapsed_ms: performance.now() - start, ...(error ? { error: safeError(error) } : {}) }
      if (secure && !error) result.protocol = socket.getProtocol()
      socket.destroy()
      resolve(result)
    }
    socket.setTimeout(2000, () => finish(Object.assign(Error(), { code: "ETIMEDOUT" })))
    socket.once(secure ? "secureConnect" : "connect", () => finish())
    socket.once("error", finish)
  })
}
export async function probe(value) {
  const url = new URL(origin(value))
  const hostname = url.hostname.replace(/^\[|\]$/g, "")
  const start = performance.now()
  const result = { origin: url.origin, at_unix_ms: Date.now(), purpose: "independent_route_probe_not_inference_connection" }
  try {
    const addresses = await deadline(lookup(hostname, { all: true }), 2000)
    result.dns = { ok: true, input_is_ip: Boolean(net.isIP(hostname)), elapsed_ms: performance.now() - start, addresses: addresses.slice(0, 8) }
    result.routes = []
    // Check both families where present; a failed IPv6 probe alone is not an outage.
    const selected = [...new Map(addresses.map(a => [a.family, a])).values()].slice(0, 2)
    for (const address of selected) {
      const port = Number(url.port || (url.protocol === "https:" ? 443 : 80))
      const tcp = await connect(address.address, port, hostname, false)
      const route = { ...address, tcp }
      if (tcp.ok && url.protocol === "https:") route.tls = await connect(address.address, port, net.isIP(hostname) ? undefined : hostname, true)
      result.routes.push(route)
    }
  } catch (error) {
    result.dns = { ok: false, elapsed_ms: performance.now() - start, error: safeError(error) }
  }
  return result
}

export function createCapture(settings) {
  const file = `${settings.work}/transport.private.jsonl`
  const statusFile = `${settings.work}/transport-capture-status.json`
  const bodyLimit = settings.body_limit_bytes ?? 1024 * 1024
  const logLimit = settings.log_limit_bytes ?? 64 * 1024 * 1024
  const requests = new WeakMap()
  const pending = new Map()
  const origins = new Set(settings.control_origins ?? [])
  const tasks = new Set()
  const secrets = new Set()
  let queue = Promise.resolve(), bytes = 0, dropped = 0, writeErrors = 0, closed = false, probing = false, drainComplete = false
  let lastProbe = -Infinity
  function write(value) {
    let line
    try { line = JSON.stringify({ format_version: 1, at_unix_ms: Date.now(), ...value }) + "\n" }
    catch { writeErrors++; return queue }
    if (bytes + Buffer.byteLength(line) > logLimit) { dropped++; return queue }
    bytes += Buffer.byteLength(line)
    queue = queue.then(() => appendFile(file, line, { mode: 0o600 })).catch(() => { writeErrors++ })
    return queue
  }
  function track(task) {
    tasks.add(task)
    void task.finally(() => tasks.delete(task)).catch(() => {})
  }
  async function status() {
    await queue
    const data = { format_version: 1, closed, bytes_scheduled: bytes, dropped_records: dropped, write_errors: writeErrors,
      pending_requests: [...pending.keys()], pending_probe_tasks: tasks.size, drain_complete: drainComplete,
      control_origins: settings.control_origins ?? [],
      limitations: ["Route probes are separate connections; they cannot prove the cause of an inference socket reset.", "HTTP hooks do not capture native WebSocket traffic."] }
    try { await writeFile(statusFile, JSON.stringify(data) + "\n", { mode: 0o600 }) } catch { writeErrors++ }
  }
  async function sample(reason) {
    if (closed || probing || performance.now() - lastProbe < 30_000) return
    probing = true; lastProbe = performance.now()
    try {
      const observations = await Promise.all([...origins].map(probe))
      await write({ type: "network_sample", reason, observations })
    } catch (error) { await write({ type: "capture_error", stage: "network_sample", error: safeError(error) }) }
    finally { probing = false; await status() }
  }
  const timer = setInterval(() => { track(sample("periodic")); void status() }, 60_000)
  timer.unref?.()
  void write({ type: "capture_started", body_limit_bytes: bodyLimit, log_limit_bytes: logLimit, control_origins: settings.control_origins ?? [] })
  async function request(event) {
    try {
      const req = event.request
      for (const [key, value] of req.headers) if (credential.test(key)) {
        secrets.add(value)
        if (value.startsWith("Bearer ")) secrets.add(value.slice(7))
      }
      const id = randomUUID(), start = performance.now()
      const state = { id, start, signal: req.signal, session_id: event.sessionID, kind: event.kind }
      requests.set(req, state); pending.set(id, state)
      origins.add(origin(req.url))
      state.abort = () => { void write({ type: "request_signal_abort", request_id: id, elapsed_ms: performance.now() - start, reason: safeError(req.signal.reason) }) }
      req.signal.addEventListener("abort", state.abort, { once: true })
      if (req.signal.aborted) state.abort()
      await write({ type: "request", request_id: id, session_id: event.sessionID, model: event.model, kind: event.kind,
        method: req.method, origin: origin(req.url), headers: headers(req.headers), omitted_header_names: [...req.headers.keys()].filter(k => !allowed.test(k)) })
      // Read a bounded clone, not the request delivered to the provider. Never
      // await cancellation of a tee branch: that can wait for the unsent original.
      if (req.body) {
        const reader = req.clone().body.getReader()
        const chunks = []; let size = 0, complete = false
        try {
          await deadline((async () => {
            while (size < bodyLimit) {
              const { value, done } = await reader.read()
              if (done) { complete = true; break }
              const part = value.subarray(0, bodyLimit - size)
              chunks.push(part); size += part.length
            }
          })(), 2000)
        } catch (error) { await write({ type: "capture_error", request_id: id, stage: "request_body", error: safeError(error) }) }
        finally { void reader.cancel().catch(() => {}) }
        const body = Buffer.concat(chunks)
        await write({ type: "request_body", request_id: id, captured_bytes: size, complete,
          captured_sha256: createHash("sha256").update(body).digest("hex"), body: complete ? redact(body.toString("utf8"), secrets) : null })
      }
      track(sample("request"))
    } catch (error) { await write({ type: "capture_error", stage: "request_hook", error: safeError(error) }) }
  }
  function response(event) {
    let reader
    try {
      const state = requests.get(event.request)
      const id = state?.id ?? randomUUID(), start = state?.start ?? performance.now()
      if (!state) void write({ type: "correlation_gap", request_id: id })
      const res = event.response
      void write({ type: "response_headers", request_id: id, elapsed_ms: performance.now() - start,
        status: res.status, headers: headers(res.headers), omitted_header_names: [...res.headers.keys()].filter(k => !allowed.test(k)) })
      if (!res.body) {
        pending.delete(id); state?.signal.removeEventListener("abort", state.abort)
        void write({ type: "stream_end", request_id: id, complete: true, bytes_read: 0, body: null }); return
      }
      reader = res.body.getReader()
      const hash = createHash("sha256"), chunks = [], identities = new Set()
      let total = 0, captured = 0, first = null, last = null, maxGap = 0, ended = false, sse = "", sseOverflows = 0, droppingFrame = false
      const decoder = new TextDecoder()
      const isSse = res.headers.get("content-type")?.includes("text/event-stream")
      function frame(text) {
        const data = text.split(/\r?\n/).filter(l => l.startsWith("data:")).map(l => l.slice(5).trimStart()).join("\n")
        if (data === "[DONE]") { void write({ type: "stream_terminal", request_id: id, marker: "DONE" }); return }
        try {
          const value = JSON.parse(data)
          const error = value.error
          if (error || value.choices?.some(c => c.finish_reason === "error")) {
            void write({ type: "provider_error_payload", request_id: id, response_id: typeof value.id === "string" ? value.id.slice(0, 256) : null,
              error: JSON.parse(redact(JSON.stringify(error ?? {}), secrets) ?? "{}"), elapsed_ms: performance.now() - start })
            track(sample("provider_error"))
          }
          if (value.id && !identities.has(value.id) && identities.size < 16) {
            identities.add(value.id)
            void write({ type: "response_identity", request_id: id, response_id: String(value.id).slice(0, 256) })
          }
        } catch { /* Non-JSON SSE frames remain visible in the bounded private body. */ }
      }
      function observe(action) {
        try { action() } catch (error) { void write({ type: "capture_error", request_id: id, stage: "stream_observer", error: safeError(error) }) }
      }
      function finish(type, error) { observe(() => {
        if (ended) return
        ended = true
        pending.delete(id); state?.signal.removeEventListener("abort", state.abort)
        const body = Buffer.concat(chunks)
        // SSE bodies contain ordinary content; retain privately, with any known
        // HTTP credentials removed. JSON error bodies get structured redaction.
        let text = body.toString("utf8")
        for (const secret of secrets) if (secret.length >= 4) text = text.replaceAll(secret, "[REDACTED]")
        void write({ type, request_id: id, elapsed_ms: performance.now() - start, first_chunk_ms: first, max_inter_chunk_gap_ms: maxGap,
          bytes_read: total, captured_bytes: captured, body_truncated: captured < total, stream_sha256: hash.digest("hex"),
          complete: type === "stream_end", body: isSse ? text : redact(text, secrets), sse_overflow_frames: sseOverflows,
          ...(error ? { error: safeError(error) } : {}) })
        if (error) track(sample("stream_error"))
        void status()
      }) }
      event.response = new Response(new ReadableStream({
        async pull(controller) {
          let chunk
          try { chunk = await reader.read() } catch (error) { finish("stream_error", error); controller.error(error); return }
          if (chunk.done) { observe(() => { if (isSse && !droppingFrame && sse.trim()) frame(sse) }); finish("stream_end"); controller.close(); return }
          observe(() => {
          const now = performance.now()
          if (first === null) { first = now - start; void write({ type: "first_chunk", request_id: id, elapsed_ms: first }) }
          if (last !== null) maxGap = Math.max(maxGap, now - last)
          last = now
          hash.update(chunk.value); total += chunk.value.byteLength
          if (captured < bodyLimit) { const part = chunk.value.slice(0, bodyLimit - captured); chunks.push(part); captured += part.length }
          if (isSse) {
            sse += decoder.decode(chunk.value, { stream: true })
            const frames = sse.split(/\r?\n\r?\n/); sse = frames.pop()
            if (droppingFrame && frames.length) { frames.shift(); droppingFrame = false }
            for (const text of frames) if (text.length <= 65536) frame(text); else sseOverflows++
            if (sse.length > 65536) { sse = ""; if (!droppingFrame) sseOverflows++; droppingFrame = true }
          }
          })
          controller.enqueue(chunk.value) // Preserve original bytes and backpressure.
        },
        cancel(reason) { finish("consumer_cancel", reason); return reader.cancel(reason) },
      }), { status: res.status, statusText: res.statusText, headers: res.headers })
    } catch (error) {
      reader?.releaseLock()
      void write({ type: "capture_error", stage: "response_hook", error: safeError(error) })
    }
  }
  async function retry(event) {
    await write({ type: "client_retry_decision", session_id: event.sessionID, model: event.model, kind: event.kind,
      attempt: event.attempt, decision: event.decision,
      pending_request_ids: [...pending.values()].filter(r => r.session_id === event.sessionID).map(r => r.id),
      error: { type: event.error?.type, status: event.error?.status, ...safeError(event.error) } })
    track(sample("retry")) // Observe only; do not change the client's retry policy.
  }
  async function close() {
    clearInterval(timer); closed = true
    try { await deadline(Promise.allSettled([...tasks]), 2500); drainComplete = true } catch { /* Report unfinished probes explicitly. */ }
    await status()
  }
  return { request, response, retry, close, flush: status }
}
