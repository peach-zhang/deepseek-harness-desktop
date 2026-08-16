import z from "@deepseek-ai/schemastery";
import { credentialRef } from "@deepseek-ai/dsh-credentials";

/**
 * @deepseek-ai/dsh-plugin-deepseek-usage — host half.
 *
 * Registers one exact HTTP route (`/deepseek-usage`) on the Web server that:
 *   1. resolves the DeepSeek API key through `ctx.credentials` (per request,
 *      so a rotated key reaches the very next query without a restart),
 *   2. calls the official balance endpoint `GET <baseURL>/user/balance`,
 *   3. returns a small JSON envelope the browser half renders.
 *
 * The key never leaves the host and never crosses the wire: the route answers
 * with balance figures only.
 */

/** Stable Cordis plugin name. */
const name = "deepseek-usage";

/** Services required before the route can be registered. */
const inject = ["webServer", "credentials"];

/** Plugin config (schema defaults applied by the loader). */
const Config = z.object({
  /** Credential reference (an env-var-shaped name) holding the DeepSeek key. */
  apiKeyEnv: z.string().default("DEEPSEEK_API_KEY"),
  /** DeepSeek API base; the balance endpoint is `<baseURL>/user/balance`. */
  baseURL: z.string().default("https://api.deepseek.com")
});

function writeJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store"
  });
  res.end(body);
}

function apply(ctx, config) {
  const apiKeyEnv = config.apiKeyEnv;
  const baseURL = config.baseURL.replace(/\/+$/, "");

  ctx.effect(() => ctx.webServer.register({
    kind: "exact",
    path: "/deepseek-usage",
    handler: async (req, res) => {
      if (req.method !== "GET") {
        writeJson(res, 405, { ok: false, error: "METHOD_NOT_ALLOWED" });
        return;
      }
      try {
        const hit = await ctx.credentials.resolve(credentialRef(apiKeyEnv));
        if (hit === undefined || hit.value === "") {
          writeJson(res, 200, {
            ok: false,
            error: "MISSING_CREDENTIAL",
            message: `API key not configured (${apiKeyEnv})`
          });
          return;
        }

        const upstream = await fetch(`${baseURL}/user/balance`, {
          method: "GET",
          headers: {
            Authorization: `Bearer ${hit.value}`,
            Accept: "application/json"
          }
        });

        const text = await upstream.text();
        let parsed = null;
        try {
          parsed = text ? JSON.parse(text) : null;
        } catch {
          parsed = null;
        }

        if (!upstream.ok) {
          const upstreamMessage =
            (parsed && (parsed.error?.message || parsed.message)) ||
            text ||
            `HTTP ${upstream.status}`;
          writeJson(res, 200, {
            ok: false,
            error: "UPSTREAM_ERROR",
            status: upstream.status,
            message: upstreamMessage
          });
          return;
        }

        writeJson(res, 200, { ok: true, status: upstream.status, data: parsed });
      } catch (err) {
        writeJson(res, 200, {
          ok: false,
          error: "TRANSPORT",
          message: err instanceof Error ? err.message : String(err)
        });
      }
    }
  }), "deepseek-usage: http route");
}

export { Config, apply, inject, name };
