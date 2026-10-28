/**
 * cdn.seavx.com — serves Seavx build artifacts out of the private `seavx` R2 bucket.
 *
 *   GET /builds/<sha>/<file>   immutable artifact, cached for a year
 *   GET /latest/<os>/<arch>    302 to the newest build for that target
 *
 * The pointer objects `latest-<os>-<arch>.json` are written by CI on every push:
 *   { "sha": "a1b2c3d", "key": "builds/a1b2c3d/seavx-linux-x86_64", "built_at": "..." }
 */
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed\n", { status: 405 });
    }

    const latest = url.pathname.match(/^\/latest\/([a-z0-9]+)\/([a-z0-9_]+)$/);
    if (latest) {
      const pointer = await env.BUCKET.get(`latest-${latest[1]}-${latest[2]}.json`);
      if (!pointer) {
        return new Response("no build published for that target\n", { status: 404 });
      }
      const { key } = await pointer.json();
      // The pointer moves, the artifact it names never does.
      return new Response(null, {
        status: 302,
        headers: { location: `${url.origin}/${key}`, "cache-control": "public, max-age=60" },
      });
    }

    const key = url.pathname.slice(1);
    if (!key.startsWith("builds/")) {
      return new Response("not found\n", { status: 404 });
    }

    const object = await env.BUCKET.get(key);
    if (!object) {
      return new Response("not found\n", { status: 404 });
    }

    const headers = new Headers();
    object.writeHttpMetadata(headers);
    if (!headers.has("content-type")) {
      headers.set("content-type", "application/octet-stream");
    }
    headers.set("etag", object.httpEtag);
    headers.set("cache-control", "public, max-age=31536000, immutable");
    return new Response(request.method === "HEAD" ? null : object.body, { headers });
  },
};
