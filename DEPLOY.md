# Deploy (Cloudflare tunnel POC)

music-api runs as a container on the **same Docker network** as the existing
`cloudflared` tunnel (from the leptos `docker-compose.prod.yml`). The tunnel
routes `musicapi.yudhyapw.com` → `http://music-api:8080`.

Prereq: the tunnel is already running (so its network exists).

### 1. Prod `.env` on the deploy host
In `music-api/.env` set the **production** values:

```
SPOTIFY_CLIENT_ID=...
SPOTIFY_CLIENT_SECRET=...
SPOTIFY_REDIRECT_URI=https://musicapi.yudhyapw.com/auth/spotify/callback
OWNER_SPOTIFY_USER_ID=...
AUTH_BASIC_USERNAME=owner
AUTH_BASIC_PASSWORD=...
```

`DATABASE_URL` and `BIND_ADDR` are set by compose — leave them out.

### 2. Confirm the tunnel network name
```
docker network ls        # default assumed: portofolio-yudhya-leptos_default
```
If different, set `TUNNEL_NETWORK=<name>` (in the shell or `.env`).

### 3. Build + run
```
cd music-api
docker compose up -d --build
```

### 4. Cloudflare dashboard (one-time)
Zero Trust → Networks → Tunnels → your tunnel → Public Hostname → Add:
- Subdomain `musicapi`, Domain `yudhyapw.com`
- Service: **HTTP**, URL: `music-api:8080`

### 5. Spotify dashboard (one-time)
Redirect URI: `https://musicapi.yudhyapw.com/auth/spotify/callback`

### Try it
- `https://musicapi.yudhyapw.com/healthz` → `{"status":"ok",...}`
- `https://musicapi.yudhyapw.com/auth/spotify/login` → Basic auth → Spotify → "Spotify linked."
- After linking, `/healthz` shows `"token_state":"authorized"`.
