import { Plugin } from "@opencode/plugin"
import { createCapture } from "./capture.mjs"
import settings from "./settings.json"

export default Plugin.define({
  id: "proofstorm.transport-capture",
  async setup(ctx) {
    const capture = createCapture(settings)
    const filter = { providerID: settings.provider_id }
    await ctx.session.hook("http.request", capture.request, filter)
    await ctx.session.hook("http.response", capture.response, filter)
    await ctx.session.hook("retry", capture.retry, filter)
    return () => capture.close()
  },
})
