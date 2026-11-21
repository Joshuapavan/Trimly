/* Makes the page feel alive: a live trimming demo you can drag, scroll reveals,
 * count-up numbers, a scroll progress bar and card glow. Everything here is
 * decoration or a demo: the page works without it, and it respects
 * "reduce motion". */
(() => {
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

  // ------------------------------------------------------------ page life
  const progress = $('.progress i');
  const header = $('.top');
  let ticking = false;
  function onScroll() {
    const max = document.documentElement.scrollHeight - innerHeight;
    if (progress) progress.style.setProperty('--p', max > 0 ? (scrollY / max).toFixed(4) : '0');
    if (header) header.classList.toggle('scrolled', scrollY > 8);
    ticking = false;
  }
  addEventListener(
    'scroll',
    () => {
      if (!ticking) {
        ticking = true;
        requestAnimationFrame(onScroll);
      }
    },
    { passive: true },
  );
  onScroll();

  // Things fade and rise into place as they scroll into view.
  if (!reduce && 'IntersectionObserver' in window) {
    const targets = $$('.card, .way, .steps li, .dl, .band, .stat, .section > h2, .sect-lead, #faq details, .req, .marquee, .support-card, .final .cta');
    targets.forEach((el) => {
      el.classList.add('reveal');
      el.style.setProperty('--i', Math.min([...el.parentElement.children].indexOf(el), 6));
    });
    const io = new IntersectionObserver(
      (entries) =>
        entries.forEach((e) => {
          if (!e.isIntersecting) return;
          const el = e.target;
          el.classList.add('in');
          io.unobserve(el);
          // Once it has arrived, drop the entrance styles so hover effects are snappy.
          setTimeout(() => {
            el.classList.remove('reveal', 'in');
            el.style.removeProperty('--i');
          }, 1200);
        }),
      { threshold: 0.12, rootMargin: '0px 0px -6% 0px' },
    );
    targets.forEach((t) => io.observe(t));
  }

  // A soft glow follows the pointer across cards.
  $$('.card, .way').forEach((c) =>
    c.addEventListener('pointermove', (e) => {
      const r = c.getBoundingClientRect();
      c.style.setProperty('--mx', e.clientX - r.left + 'px');
      c.style.setProperty('--my', e.clientY - r.top + 'px');
    }),
  );

  // Numbers count up the first time you see them.
  const countUp = (el) => {
    const to = +el.dataset.count;
    const pre = el.dataset.prefix || '';
    const suf = el.dataset.suffix || '';
    const t0 = performance.now();
    const tick = (now) => {
      const k = Math.min(1, (now - t0) / 1300);
      el.textContent = pre + Math.round(to * (1 - Math.pow(1 - k, 3))).toLocaleString('en-US') + suf;
      if (k < 1) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  };
  if (!reduce && 'IntersectionObserver' in window) {
    const cio = new IntersectionObserver(
      (entries) =>
        entries.forEach((e) => {
          if (e.isIntersecting) {
            countUp(e.target);
            cio.unobserve(e.target);
          }
        }),
      { threshold: 0.6 },
    );
    $$('[data-count]').forEach((el) => {
      el.textContent = (el.dataset.prefix || '') + '0' + (el.dataset.suffix || '');
      cio.observe(el);
    });
  }

  // ------------------------------------------------------------ the live demo
  const demo = $('#demo');
  if (!demo) return;

  const TOTAL = 3492; // a pretend 58 minute video, in seconds
  const GAP = 5;
  const FINAL = { start: 765, end: 2110 }; // 12:45 to 35:10
  const E = (id) => document.getElementById(id);
  const el = {
    url: E('demo-url'), load: E('demo-load'), frame: E('demo-frame'), time: E('demo-time'), chip: E('demo-chip'),
    len: E('demo-len'), size: E('demo-size'), track: E('demo-track'), thumbs: E('demo-thumbs'),
    dimL: E('demo-dim-l'), dimR: E('demo-dim-r'), sel: E('demo-sel'), hs: E('demo-hs'), he: E('demo-he'),
    ts: E('demo-ts'), te: E('demo-te'), hint: E('demo-hint'), dl: E('demo-dl'), dlLabel: E('demo-dl-label'),
    fill: $('.fill', E('demo-dl')), replay: E('demo-replay'),
  };

  let start = 0;
  let end = TOTAL;
  let cursor = 0; // the moment shown in the picture
  let token = 0; // changes when the demo is interrupted, which stops any running animation
  let auto = true;
  let visible = true;

  const pad = (n) => String(n).padStart(2, '0');
  const fmt = (s) => {
    s = Math.round(Math.max(0, s));
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return h ? `${h}:${pad(m)}:${pad(s % 60)}` : `${pad(m)}:${pad(s % 60)}`;
  };

  // Filmstrip: little pictures that change along the video.
  for (let i = 0; i < 30; i++) {
    const t = document.createElement('i');
    t.style.setProperty('--c', String(18 + (i / 29) * 200));
    el.thumbs.appendChild(t);
  }

  function scene(t) {
    const r = t / TOTAL;
    el.frame.style.setProperty('--h', String(18 + r * 200));
    el.frame.style.setProperty('--h2', String(40 + r * 40));
    el.frame.style.setProperty('--h3', String(250 + r * 70));
    el.frame.style.setProperty('--sx', 12 + r * 76 + '%');
    el.time.textContent = fmt(t);
  }

  function render() {
    const ps = (start / TOTAL) * 100;
    const pe = (end / TOTAL) * 100;
    el.hs.style.left = ps + '%';
    el.he.style.left = pe + '%';
    el.sel.style.left = ps + '%';
    el.sel.style.width = pe - ps + '%';
    el.dimL.style.width = ps + '%';
    el.dimR.style.left = pe + '%';
    el.dimR.style.right = '0';
    el.ts.textContent = fmt(start);
    el.te.textContent = fmt(end);
    el.hs.setAttribute('aria-valuenow', String(Math.round(start)));
    el.hs.setAttribute('aria-valuemax', String(Math.round(end)));
    el.he.setAttribute('aria-valuenow', String(Math.round(end)));
    el.he.setAttribute('aria-valuemin', String(Math.round(start)));
    el.he.setAttribute('aria-valuemax', String(TOTAL));
    el.len.textContent = fmt(end - start);
    const loaded = demo.classList.contains('demo--loaded');
    el.size.textContent = loaded ? Math.max(1, Math.round((end - start) * 0.11)) + ' MB' : '0 MB';
    scene(cursor);
  }

  function flashStats() {
    [el.len, el.size].forEach((b) => {
      b.classList.add('flash');
      setTimeout(() => b.classList.remove('flash'), 350);
    });
  }

  // ---- you can drag it
  let drag = null;
  const timeAt = (ev) => {
    const r = el.track.getBoundingClientRect();
    return Math.round(Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width)) * TOTAL);
  };
  function moveTo(t) {
    if (drag === 's') {
      start = Math.min(t, end - GAP);
      cursor = start;
    } else {
      end = Math.max(t, start + GAP);
      cursor = end;
    }
    render();
    flashStats();
  }
  function takeOver() {
    auto = false;
    token++;
    el.replay.hidden = false;
    el.hint.textContent = 'Your turn: drag the orange handles';
    el.dl.classList.remove('busy', 'done');
    el.fill.style.width = '0';
    el.dlLabel.textContent = 'Download clip';
  }
  el.track.addEventListener('pointerdown', (ev) => {
    if (!demo.classList.contains('demo--loaded')) return;
    takeOver();
    const t = timeAt(ev);
    const grabbed = ev.target.closest('.d-hd');
    drag = grabbed ? (grabbed === el.hs ? 's' : 'e') : Math.abs(t - start) <= Math.abs(t - end) ? 's' : 'e';
    (drag === 's' ? el.hs : el.he).classList.add('drag');
    el.track.setPointerCapture(ev.pointerId);
    moveTo(t);
  });
  el.track.addEventListener('pointermove', (ev) => {
    if (drag) moveTo(timeAt(ev));
  });
  const release = () => {
    el.hs.classList.remove('drag');
    el.he.classList.remove('drag');
    drag = null;
  };
  el.track.addEventListener('pointerup', release);
  el.track.addEventListener('pointercancel', release);
  [el.hs, el.he].forEach((h) =>
    h.addEventListener('keydown', (ev) => {
      const step = ev.shiftKey ? 30 : 5;
      const dir = ev.key === 'ArrowLeft' || ev.key === 'ArrowDown' ? -1 : ev.key === 'ArrowRight' || ev.key === 'ArrowUp' ? 1 : 0;
      if (!dir) return;
      ev.preventDefault();
      takeOver();
      drag = h === el.hs ? 's' : 'e';
      moveTo((drag === 's' ? start : end) + dir * step);
      drag = null;
    }),
  );

  // ---- "Download clip" works too
  async function save(my) {
    el.dl.classList.remove('done');
    el.dl.classList.add('busy');
    const ok = await tween(0, 100, 1500, (v) => {
      el.fill.style.width = v + '%';
      el.dlLabel.textContent = 'Downloading ' + Math.round(v) + '%';
    }, my);
    if (!ok) return;
    el.dl.classList.remove('busy');
    el.dl.classList.add('done');
    el.dlLabel.textContent = 'Saved · ' + fmt(end - start);
    flashStats();
  }
  const clickSave = () => {
    if (!demo.classList.contains('demo--loaded')) return;
    takeOver();
    el.hint.textContent = 'Only that part is downloaded';
    save(token);
  };
  el.dl.addEventListener('click', clickSave);
  el.dl.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter' || ev.key === ' ') {
      ev.preventDefault();
      clickSave();
    }
  });

  // ---- the self-playing demo
  const sleep = (my, ms) => new Promise((r) => setTimeout(() => r(my === token), ms));
  function tween(from, to, ms, fn, my) {
    const ease = (k) => (k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2);
    return new Promise((res) => {
      const t0 = performance.now();
      const frame = (now) => {
        if (my !== token) return res(false);
        const k = Math.min(1, (now - t0) / ms);
        fn(from + (to - from) * ease(k));
        if (k < 1) requestAnimationFrame(frame);
        else res(true);
      };
      requestAnimationFrame(frame);
    });
  }

  function resetState() {
    start = 0;
    end = TOTAL;
    cursor = 0;
    demo.classList.remove('demo--loaded', 'demo--cropped');
    el.url.textContent = '';
    el.chip.textContent = 'Original';
    el.dl.classList.remove('busy', 'done');
    el.fill.style.width = '0';
    el.dlLabel.textContent = 'Download clip';
    el.frame.style.setProperty('--cx', '34%');
    el.hint.textContent = 'Watch it trim a clip';
    render();
  }

  function finalState() {
    demo.classList.add('demo--loaded', 'demo--cropped');
    start = FINAL.start;
    end = FINAL.end;
    cursor = start;
    el.url.textContent = 'youtube.com/watch?v=trimly-demo';
    el.chip.textContent = '9:16 · Reels';
    el.hint.textContent = 'Drag the orange handles to trim';
    render();
  }

  async function run() {
    const my = ++token;
    resetState();
    while (!visible) if (!(await sleep(my, 300))) return;
    if (!(await sleep(my, 800))) return;

    const text = 'youtube.com/watch?v=trimly-demo';
    for (let i = 1; i <= text.length; i++) {
      el.url.textContent = text.slice(0, i);
      if (!(await sleep(my, 42))) return;
    }
    if (!(await sleep(my, 350))) return;
    el.load.classList.add('press');
    if (!(await sleep(my, 170))) return;
    el.load.classList.remove('press');
    el.hint.textContent = 'Reading the link…';
    if (!(await sleep(my, 1000))) return;

    demo.classList.add('demo--loaded');
    render();
    el.hint.textContent = 'Picking the part to keep…';
    if (!(await sleep(my, 700))) return;

    if (!(await tween(0, FINAL.start, 1400, (v) => { start = v; cursor = v; render(); }, my))) return;
    if (!(await tween(TOTAL, FINAL.end, 1400, (v) => { end = v; cursor = v; render(); }, my))) return;
    flashStats();

    el.hint.textContent = 'Crop it for Reels';
    demo.classList.add('demo--cropped');
    el.chip.textContent = '9:16 · Reels';
    const cx = (v) => el.frame.style.setProperty('--cx', v + '%');
    if (!(await tween(34, 16, 900, cx, my))) return;
    if (!(await tween(16, 52, 1200, cx, my))) return;
    if (!(await tween(52, 34, 800, cx, my))) return;

    el.hint.textContent = 'Download just this part';
    if (!(await sleep(my, 600))) return;
    await save(my);
    if (my !== token) return;
    el.hint.textContent = 'Saved. Try it yourself: drag a handle';
    if (!(await sleep(my, 3600))) return;
    if (auto) run();
  }

  el.replay.addEventListener('click', () => {
    auto = true;
    el.replay.hidden = true;
    run();
  });

  if ('IntersectionObserver' in window) {
    new IntersectionObserver((entries) => entries.forEach((e) => (visible = e.isIntersecting)), { threshold: 0.2 }).observe(demo);
  }

  // The demo plays on its own, unless the visitor prefers less motion.
  if (reduce) {
    auto = false;
    finalState();
  } else {
    run();
  }
})();
