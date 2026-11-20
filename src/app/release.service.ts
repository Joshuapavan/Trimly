import { Injectable, signal } from '@angular/core';

declare global {
  interface Window {
    /** The GitHub repository (owner/name) whose releases hold the desktop installers. */
    TRIMLY_REPO?: string;
  }
}

export type DownloadId = 'mac-arm' | 'mac-x64' | 'win' | 'linux-appimage' | 'linux-deb';

export interface Download {
  id: DownloadId;
  system: string;
  detail: string;
  /** The installer file, or the Releases page if it couldn't be found. */
  url: string;
  size: string | null;
}

interface Asset {
  name: string;
  size: number;
  browser_download_url: string;
}

interface Release {
  tag_name: string;
  draft: boolean;
  published_at: string | null;
  assets: Asset[];
}

const OPTIONS: { id: DownloadId; system: string; detail: string; test: (name: string) => boolean }[] = [
  { id: 'mac-arm', system: 'macOS', detail: 'Apple silicon (M1 and newer)', test: (n) => /\.dmg$/i.test(n) && /(aarch64|arm64)/i.test(n) },
  { id: 'mac-x64', system: 'macOS', detail: 'Intel', test: (n) => /\.dmg$/i.test(n) && /(x64|x86_64|intel)/i.test(n) },
  { id: 'win', system: 'Windows', detail: '10 and 11, 64-bit', test: (n) => /\.exe$/i.test(n) },
  { id: 'linux-appimage', system: 'Linux', detail: 'AppImage, runs anywhere', test: (n) => /\.appimage$/i.test(n) },
  { id: 'linux-deb', system: 'Linux', detail: 'Debian and Ubuntu (.deb)', test: (n) => /\.deb$/i.test(n) },
];

const size = (bytes: number) => (bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + ' GB' : Math.max(1, Math.round(bytes / 1e6)) + ' MB');

/**
 * Finds the newest GitHub release and which installer suits this computer.
 * Pre-releases count, because every build from the main branch is published as one.
 */
@Injectable({ providedIn: 'root' })
export class ReleaseService {
  readonly repo = (typeof window !== 'undefined' && window.TRIMLY_REPO) || 'Joshuapavan/Trimly';
  readonly releasesUrl = `https://github.com/${this.repo}/releases`;

  readonly loading = signal(false);
  readonly failed = signal(false);
  readonly version = signal('');
  readonly downloads = signal<Download[]>(OPTIONS.map((o) => ({ id: o.id, system: o.system, detail: o.detail, url: this.releasesUrl, size: null })));
  /** The installer that fits this computer, if we can tell. */
  readonly mine = signal<DownloadId | null>(null);

  private started = false;

  /** Look up the release. Safe to call more than once; it only asks GitHub the first time. */
  async load(): Promise<void> {
    this.mine.set(await this.detect());
    if (this.started) return;
    this.started = true;
    this.loading.set(true);
    try {
      const res = await fetch(`https://api.github.com/repos/${this.repo}/releases?per_page=10`, {
        headers: { Accept: 'application/vnd.github+json' },
      });
      if (!res.ok) throw new Error(String(res.status));
      const release = ((await res.json()) as Release[]).find((r) => !r.draft);
      if (!release) throw new Error('no release');
      this.version.set(release.tag_name);
      this.downloads.set(
        OPTIONS.map((o) => {
          const asset = release.assets.find((a) => o.test(a.name));
          return { id: o.id, system: o.system, detail: o.detail, url: asset?.browser_download_url ?? this.releasesUrl, size: asset ? size(asset.size) : null };
        }),
      );
    } catch {
      // Offline, rate limited, or no release yet: the links open the Releases page.
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  private async detect(): Promise<DownloadId | null> {
    const ua = navigator.userAgent || '';
    const nav = navigator as Navigator & { userAgentData?: { platform?: string; getHighEntropyValues(h: string[]): Promise<{ architecture?: string }> } };
    const plat = nav.userAgentData?.platform || navigator.platform || '';
    if (/android|iphone|ipad|ipod/i.test(ua)) return null;
    if (/mac/i.test(plat) || /Macintosh/.test(ua)) {
      let arch: string | undefined;
      try {
        arch = (await nav.userAgentData?.getHighEntropyValues(['architecture']))?.architecture;
      } catch {
        /* not available in Safari or Firefox */
      }
      return arch === 'x86' ? 'mac-x64' : 'mac-arm';
    }
    if (/win/i.test(plat) || /Windows/.test(ua)) return 'win';
    if (/linux|x11|cros/i.test(plat + ua)) return 'linux-appimage';
    return null;
  }
}
