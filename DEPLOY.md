# Deploy (Cloudflare tunnel POC)

Deploy from the **repo root** (`portofolio_services`), not this dir — the
parent `.env` is the single source of truth and `docker-compose.yml` lives
there. music-api joins the existing `cloudflared` tunnel network so
`musicapi.yudhyapw.com` → `http://music-api:8080`.

Prereq: the tunnel (leptos stack) is already running, so its network exists.

### 1. Parent `.env` (one source of truth)
In `portofolio_services/.env` (copy from `.env.example`) set:

```
SPOTIFY_CLIENT_ID=...
SPOTIFY_CLIENT_SECRET=...
SPOTIFY_REDIRECT_URI=https://musicapi.yudhyapw.com/auth/spotify/callback
OWNER_SPOTIFY_USER_ID=...
AUTH_BASIC_USERNAME=owner
AUTH_BASIC_PASSWORD=...
```

### 2. Confirm the tunnel network name
```
docker network ls        # default assumed: portofolio-yudhya-leptos_default
```
If different, set `TUNNEL_NETWORK=<name>` in the parent `.env`.

### 3. Build + run (from the repo root)
```
docker compose up -d --build
```

### 4. Cloudflare dashboard (one-time)
Zero Trust → Networks → Tunnels → your tunnel → Public Hostname → Add:
- Subdomain `musicapi`, Domain `yudhyapw.com`
- Service **HTTP**, URL `music-api:8080`

### 5. Spotify dashboard (one-time)
Redirect URI: `https://musicapi.yudhyapw.com/auth/spotify/callback`

### Try it
- `https://musicapi.yudhyapw.com/healthz` → `{"status":"ok",...}`
- `https://musicapi.yudhyapw.com/auth/spotify/login` → Basic auth → Spotify → "Spotify linked."

---
Local dev (`cargo run`) still uses `music-api/.env` (see `.env.example` here).
