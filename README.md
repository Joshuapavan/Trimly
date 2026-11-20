# Trimly

Clip a section of any video link and download **only that part**. Paste a link, drag a start and end, optionally crop to a vertical frame for Reels, Shorts or TikTok, and Trimly saves just the clip. It never downloads the whole video.

Built with Angular 21 and Tauri 2 (Rust). Downloading and cutting are done by [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [ffmpeg](https://ffmpeg.org).

## Features

- **Trim before you download:** a timeline with two handles, or type exact times (`12:45`, `1:02:03`).
- **Frame for reels:** crop to 9:16, 1:1 or 4:5 with a draggable box, with the output size shown.
- **Download queue:** add several clips, run a few at a time, pause and resume, cancel, retry, and open the file in its folder.
- **Fast, in parts:** long video clips are split into parts that download at the same time through a small local proxy, then joined. A three-minute test clip took about 15 seconds instead of 95.
- **Video or audio:** MP4 up to the quality you pick, or MP3, M4A and Opus audio.
- **Live streams:** pick a range counted from the start of the stream.
- **Private videos:** optionally borrow your browser's sign-in (Settings).
- **Exact cuts:** re-encodes a few seconds around each cut so the clip starts where you chose (can be turned off).
- **Light and dark theme**, remembered between launches.

## Requirements

Trimly looks for these on your computer. If one is missing, the app shows an install screen.

| Tool   | macOS                   | Windows                          | Linux                       |
| ------ | ----------------------- | -------------------------------- | --------------------------- |
| yt-dlp | `brew install yt-dlp` | `winget install yt-dlp.yt-dlp` | `pipx install yt-dlp`     |
| ffmpeg | `brew install ffmpeg` | `winget install Gyan.FFmpeg`   | `sudo apt install ffmpeg` |

You can point Trimly at custom locations in **Settings → Tools**.

## Install

Download the installer for your system from the [Releases](../../releases) page:

- macOS: `.dmg` (Apple silicon and Intel)
- Windows: `.exe`
- Linux: `.deb` or `.AppImage`

The macOS builds are not signed. The first time, right-click the app and choose **Open**, or run:

```bash
xattr -dr com.apple.quarantine /Applications/Trimly.app
```

## Development

You need Node 22, [Rust](https://rustup.rs), the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your system, and yt-dlp and ffmpeg.

```bash
npm install
npm run tauri dev      # desktop app with live reload
npm start              # UI only, in a browser, with simulated downloads
npm test               # frontend unit tests
cargo test --workspace  # Rust tests: engine, server, desktop app
npm run tauri build    # installers for your system
```

In browser mode (`npm start`) a small simulator stands in for the Rust backend, so you can work on the UI without the desktop shell. Add `?missing` to the address to see the tools-missing screen. Put `live` in the link to try a live stream, or `fail` to see an error.

### Project layout

```
src/app/              Angular UI, shared by the desktop app and the web version
  backend.service.ts  talks to the desktop engine, a Trimly server, or a demo simulator
  app.*               screens: start, clip, downloading, done, queue, settings
  timeline.ts         trim handles, zoom and pan
  preview.ts          picture at the moment you point at
  crop-editor.ts      reels crop box
  queue.service.ts    runs clips a few at a time, pause and resume
crates/engine/        the engine: yt-dlp and ffmpeg, parts, proxy, crop, pause (no UI code)
src-tauri/            the desktop app: a thin Tauri layer over the engine
server/               the web server: the same engine behind an HTTP API
website/              the download page
Dockerfile            web server + web app in one image
vercel.json           settings for deploying the web app on Vercel
.github/workflows/    CI, releases, Docker image, website deploy
```

### How a clip is made

1. yt-dlp downloads only the chosen time range (`--download-sections`).
2. For a crop, ffmpeg then re-encodes the clip with a `crop` filter for the chosen shape.
3. Progress is sent to the UI as events; each clip has an id so several can run and be cancelled separately.

## Web version

The web version has the same screens and features as the desktop app: trim, preview, zoom, crop for reels, queue, pause and resume. A browser can't run yt-dlp or ffmpeg, or fetch video from other sites on its own, so a **Trimly server** does the downloading and cutting. When a clip is finished, the browser downloads the file from the server.

```
Browser (web app)  ──►  Trimly server (engine + yt-dlp + ffmpeg)  ──►  video sites
```

The web app finds the server by itself:

- If the page was served by the Trimly server (the Docker image does this), it uses that.
- If `window.TRIMLY_API` is set in `config.js` (see Vercel below), it uses that address.
- If neither works, it runs in **demo mode** with simulated downloads and shows a "Demo mode" label.

### Run the server yourself

```bash
docker build -t trimly .
docker run -p 8080:8080 -v trimly-data:/data -e TRIMLY_ACCESS_KEY=choose-a-secret trimly
```

Open http://localhost:8080. The image contains the server, the web app, yt-dlp, ffmpeg and a JavaScript runtime. Without Docker, install yt-dlp and ffmpeg, then:

```bash
npm run build && TRIMLY_STATIC_DIR=dist/trimly/browser cargo run --release -p trimly-server
```

For development, run `cargo run -p trimly-server` and `npm run start:web` (the Angular dev server forwards `/api` to the Trimly server on port 8787).

Settings are environment variables, all optional:

| Variable | Meaning | Default |
| --- | --- | --- |
| `PORT` | Port to listen on | `8787` (`8080` in Docker) |
| `TRIMLY_ACCESS_KEY` | If set, everyone must enter this key. **Set it on any public server.** | none |
| `TRIMLY_ALLOW_ORIGIN` | Comma-separated web addresses allowed to call the API (CORS) | any |
| `TRIMLY_DATA_DIR` | Where finished clips are kept | `./data` |
| `TRIMLY_STATIC_DIR` | A folder of web files to serve (the built web app) | none |
| `TRIMLY_MAX_JOBS` | Clips running at once | `4` |
| `TRIMLY_MAX_PARTS` | Parts one clip may download at once | `4` |
| `TRIMLY_MAX_CLIP_SECS` | Longest clip allowed | `1800` |
| `TRIMLY_FILE_TTL_MINS` | How long finished clips stay available | `120` |
| `TRIMLY_COOKIES_FILE` | A cookies file (Netscape format) for yt-dlp | none |
| `TRIMLY_ALLOW_PRIVATE` | `1` allows links to private network addresses | off |

The server refuses links that point at private or local network addresses, so it can't be used to reach machines on its own network.

### Deploy the web app on Vercel

Vercel hosts the web app (static files). It **can't run the server**: its functions are short-lived, have no disk to keep clips on, and can't run a long ffmpeg job. So run the Trimly server on a host that allows long-running processes (for example Fly.io, Railway, Render or any VPS) using the Docker image, then point the Vercel site at it:

1. Deploy the Docker image somewhere with HTTPS, for example `https://trimly-api.example.com`. Give it at least 1 GB of memory and a persistent volume for `/data`. Set `TRIMLY_ACCESS_KEY`, and set `TRIMLY_ALLOW_ORIGIN` to your Vercel address.
2. In Vercel, import this repository. [`vercel.json`](vercel.json) already sets the install, build and output settings.
3. Under **Settings → Environment Variables**, add `TRIMLY_API_URL` with the address from step 1 (no trailing slash), then deploy. The build writes it into `config.js`.

#### Example: the server on Fly.io

[`fly.toml`](fly.toml) is ready to use. It builds the image from the Dockerfile, keeps one machine always running (clips in progress live in its memory), and puts clips on a volume.

```bash
fly launch --copy-config --no-deploy       # choose your own app name when asked
fly volumes create trimly_data --size 3 --region bom
fly secrets set TRIMLY_ACCESS_KEY="choose-a-long-secret"
fly deploy
```

Then set `TRIMLY_API_URL` in Vercel to `https://<your-app-name>.fly.dev` and redeploy. Edit `TRIMLY_ALLOW_ORIGIN` in `fly.toml` to your Vercel address. Keep it at exactly one machine, because a second one wouldn't know about the first one's clips.

#### Free option: run the server on your own computer

If you don't want to pay for hosting, [`scripts/share-server.sh`](scripts/share-server.sh) runs the server on your computer and shares it with a free [Cloudflare quick tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/). It needs `cloudflared`, `yt-dlp` and `ffmpeg` installed.

```bash
TRIMLY_ALLOW_ORIGIN=https://your-site.vercel.app scripts/share-server.sh
```

It prints a `https://….trycloudflare.com` address and an access key (kept in `~/.trimly-access-key`). Set that address as `TRIMLY_API_URL` in Vercel and redeploy. Two limits: the address changes every time you start it, so Vercel's variable has to be updated each time, and the site only works while your computer is on and the script is running. A home connection is also much less likely to be blocked by video sites than a cloud server.

The image is published to `ghcr.io/<owner>/<repo>` by [`.github/workflows/docker.yml`](.github/workflows/docker.yml) when you push to `main`.

### Things to know before hosting it for other people

- **Video sites often block servers.** YouTube in particular tends to refuse requests from cloud and datacenter addresses ("Sign in to confirm you're not a bot"). Providing a cookies file with `TRIMLY_COOKIES_FILE`, or running the server from a home or office connection, can help. It is not guaranteed.
- **Rules and rights.** Many sites' terms forbid downloading, and not everything may be copied. Running a public downloader service can carry legal and hosting-provider risks. Keep `TRIMLY_ACCESS_KEY` set, and only offer it to people you trust to use it responsibly.
- **Cost.** Every clip uses the server's bandwidth, processor and disk. The limits above keep a small server from being overwhelmed, but there is no per-person quota.
- **Storage is temporary.** Finished clips are deleted after `TRIMLY_FILE_TTL_MINS`. Paused clips are kept for 6 hours.
- The desktop-only options (save folder, file name pattern, browser sign-in, tool paths) are hidden in the web version.

## Website

The download page lives in [`website/`](website). It is plain HTML, CSS and JavaScript with no build step. On load it asks the GitHub API for the newest release (pre-releases included, because every build from `main` is published as one) and links the installer for the visitor's system. If GitHub can't be reached, the buttons open the Releases page instead.

Preview it locally:

```bash
python3 -m http.server 4300 --directory website
```

[`.github/workflows/pages.yml`](.github/workflows/pages.yml) deploys it to GitHub Pages whenever `website/` changes on `main`, and points the buttons at the repository it runs in. One-time setup: in the repository, open **Settings → Pages** and set **Source** to **GitHub Actions**. The default repository name in [`website/config.js`](website/config.js) is only used when previewing locally.

## Troubleshooting

- **"yt-dlp not found":** install it (see Requirements) or set its path in Settings, then choose Check again.
- **"Sign in to confirm you're not a bot" or a private video:** in Settings, pick a browser under "Use my browser's sign-in", then try again.
- **Cropped clips take longer:** cropping re-encodes the video. Plain trims are faster.

## License

No license has been chosen yet. Add a `LICENSE` file before sharing the code publicly.
