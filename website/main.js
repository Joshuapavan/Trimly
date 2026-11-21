/* Download buttons: find the newest GitHub release and link its installers. */
(() => {
  const REPO = window.TRIMLY_REPO || 'Joshuapavan/Trimly';
  const RELEASES = `https://github.com/${REPO}/releases`;
  const API = `https://api.github.com/repos/${REPO}/releases?per_page=10`;

  // How to recognise each installer by its file name.
  const TARGETS = {
    'mac-arm': (n) => /\.dmg$/i.test(n) && /(aarch64|arm64)/i.test(n),
    'mac-x64': (n) => /\.dmg$/i.test(n) && /(x64|x86_64|intel)/i.test(n),
    win: (n) => /\.exe$/i.test(n),
    'linux-appimage': (n) => /\.appimage$/i.test(n),
    'linux-deb': (n) => /\.deb$/i.test(n),
  };

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const size = (bytes) => (bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + ' GB' : Math.max(1, Math.round(bytes / 1e6)) + ' MB');

  // ---- which computer is this?
  async function detect() {
    const ua = navigator.userAgent || '';
    const plat = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
    if (/android|iphone|ipad|ipod/i.test(ua)) return { os: 'mobile' };
    if (/mac/i.test(plat) || /Macintosh/.test(ua)) {
      let arch = null;
      try {
        const hi = await navigator.userAgentData.getHighEntropyValues(['architecture']);
        arch = hi.architecture; // "arm" or "x86"
      } catch (e) {
        /* not available in Safari or Firefox */
      }
      return { os: 'mac', id: arch === 'x86' ? 'mac-x64' : 'mac-arm', sure: arch !== null };
    }
    if (/win/i.test(plat) || /Windows/.test(ua)) return { os: 'win', id: 'win' };
    if (/linux|x11|cros/i.test(plat + ua)) return { os: 'linux', id: 'linux-appimage' };
    return { os: 'other' };
  }

  async function latestRelease() {
    try {
      const cached = JSON.parse(sessionStorage.getItem('trimly.release') || 'null');
      if (cached && Date.now() - cached.at < 10 * 60 * 1000) return cached.release;
    } catch (e) {
      /* ignore */
    }
    const res = await fetch(API, { headers: { Accept: 'application/vnd.github+json' } });
    if (!res.ok) throw new Error('GitHub answered ' + res.status);
    // The newest release that isn't a draft. Pre-releases count, because every
    // build from the main branch is published as one.
    const release = (await res.json()).find((r) => !r.draft) || null;
    try {
      sessionStorage.setItem('trimly.release', JSON.stringify({ at: Date.now(), release }));
    } catch (e) {
      /* ignore */
    }
    return release;
  }

  function findAssets(release) {
    const found = {};
    for (const a of (release && release.assets) || []) {
      for (const [id, test] of Object.entries(TARGETS)) {
        if (!found[id] && test(a.name)) found[id] = a;
      }
    }
    return found;
  }

  function fillLinks(assets, release) {
    $$('[data-dl]').forEach((el) => {
      const a = assets[el.dataset.dl];
      el.href = a ? a.browser_download_url : RELEASES;
      const note = $('[data-note]', el);
      if (note) note.textContent = a ? size(a.size) : 'See all downloads';
      if (!a) el.dataset.missing = 'true';
    });
    $$('[data-version]').forEach((el) => {
      el.textContent = release ? release.tag_name : '';
      el.hidden = !release;
    });
    $$('[data-date]').forEach((el) => {
      if (release && release.published_at) {
        el.textContent = new Date(release.published_at).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
      }
    });
    $$('[data-all-releases]').forEach((el) => (el.href = RELEASES));
    $$('[data-repo]').forEach((el) => (el.href = `https://github.com/${REPO}`));
  }

  function fillHero(me, assets) {
    const btn = $('#hero-download');
    const cap = $('#hero-caption');
    if (me.os === 'mobile') {
      btn.textContent = 'See downloads';
      btn.href = '#download';
      cap.textContent = 'Trimly is a desktop app. Open this page on your computer to install it.';
      return;
    }
    if (!me.id) {
      btn.textContent = 'Choose your download';
      btn.href = '#download';
      return;
    }
    const a = assets[me.id];
    btn.href = a ? a.browser_download_url : RELEASES;
    btn.textContent = 'Download for ' + { mac: 'macOS', win: 'Windows', linux: 'Linux' }[me.os];
    const kind = { 'mac-arm': 'Apple silicon', 'mac-x64': 'Intel', win: '64-bit', 'linux-appimage': 'AppImage' }[me.id];
    const guess = me.os === 'mac' && !me.sure ? 'Apple silicon (Intel Mac? pick it below)' : kind;
    cap.textContent = [guess, a ? size(a.size) : 'on GitHub'].join(' · ');
    const row = $(`[data-dl="${me.id}"]`);
    if (row) row.classList.add('mine');
  }

  // The "online" buttons open the web app (set in config.js). Without one they are hidden.
  function hydrateWebApp() {
    const app = (window.TRIMLY_WEB_APP || '').trim();
    $$('[data-webapp]').forEach((el) => {
      if (/^(https:\/\/|\/(?!\/))/i.test(app)) {
        el.href = app;
        el.rel = 'noopener';
      } else {
        el.hidden = true;
      }
    });
    $$('[data-issues]').forEach((el) => (el.href = `https://github.com/${REPO}/issues`));
  }

  async function main() {
    hydrateWebApp();
    const me = await detect();
    $$('[data-os-only]').forEach((el) => (el.hidden = el.dataset.osOnly !== me.os));
    let release = null;
    let assets = {};
    try {
      release = await latestRelease();
      assets = findAssets(release);
    } catch (e) {
      /* no network, or rate limited: buttons fall back to the Releases page */
    }
    fillLinks(assets, release);
    fillHero(me, assets);
    document.documentElement.dataset.loaded = 'true';
  }

  // ---- small helpers
  $$('[data-copy]').forEach((b) =>
    b.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(b.dataset.copy);
        const old = b.textContent;
        b.textContent = 'Copied';
        setTimeout(() => (b.textContent = old), 1500);
      } catch (e) {
        /* clipboard blocked */
      }
    }),
  );

  const KEY = 'trimly.site.theme';
  const root = document.documentElement;
  const toggle = $('#theme');
  const apply = (t) => {
    root.dataset.theme = t;
    if (toggle) {
      toggle.setAttribute('aria-label', t === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
      toggle.dataset.mode = t;
    }
  };
  let saved = null;
  try {
    saved = localStorage.getItem(KEY);
  } catch (e) {
    /* ignore */
  }
  apply(saved === 'light' || saved === 'dark' ? saved : matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  if (toggle) {
    toggle.addEventListener('click', () => {
      const next = root.dataset.theme === 'dark' ? 'light' : 'dark';
      apply(next);
      try {
        localStorage.setItem(KEY, next);
      } catch (e) {
        /* ignore */
      }
    });
  }
  const year = $('#year');
  if (year) year.textContent = new Date().getFullYear();

  main();
})();
