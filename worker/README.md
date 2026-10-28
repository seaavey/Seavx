# cdn.seavx.com worker

Serves Seavx build artifacts out of the private R2 bucket `seavx`.

| Path | Behaviour |
|---|---|
| `GET /builds/<sha>/<file>` | streams the object with `cache-control: immutable` |
| `GET /latest/<os>/<arch>` | `302` to the newest build (`linux/x86_64`, `windows/x86_64`) |
| anything else | `404` |

CI writes `builds/<sha>/seavx-<os>-<arch>` plus the pointer `latest-<os>-<arch>.json` on every
push to master, so `/latest/...` is a cheap lookup and every artifact URL is immutable.

## Deploy

Deployed as a module worker through the Cloudflare API (R2 binding `BUCKET` → `seavx`,
route `cdn.seavx.com/*` on zone `seavx.com`):

```sh
ACCT=1126f8558285eb27bff16ec7fb835a75
curl -X PUT "https://api.cloudflare.com/client/v4/accounts/$ACCT/workers/scripts/seavx-cdn" \
  -H "Authorization: Bearer $CF_OAUTH_TOKEN" \
  -F 'metadata={"main_module":"index.js","compatibility_date":"2026-10-01","bindings":[{"type":"r2_bucket","name":"BUCKET","bucket_name":"seavx"}]};type=application/json' \
  -F 'index.js=@index.js;type=application/javascript+module'
```

The route is created once:

```sh
curl -X POST "https://api.cloudflare.com/client/v4/zones/c881d68a5a1778426d10606b4e40489a/workers/routes" \
  -H "Authorization: Bearer $CF_OAUTH_TOKEN" -H 'content-type: application/json' \
  --data '{"pattern":"cdn.seavx.com/*","script":"seavx-cdn"}'
```

Verify with `curl -sI https://cdn.seavx.com/latest/linux/x86_64` (expect `302`).
