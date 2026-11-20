import { Injectable, signal } from '@angular/core';

export interface Settings {
  folder: string;
  /** Max video height; null = best available. */
  quality: number | null;
  template: string;
  exact: boolean;
  cookies: string;
  audioFormat: 'mp3' | 'm4a' | 'opus';
  /** How many clips download at the same time. */
  concurrent: number;
  /** How many parts of a long clip download at once; 1 turns it off. */
  parts: number;
  ytdlpPath: string;
  ffmpegPath: string;
}

export interface RecentClip {
  title: string;
  file: string;
  start: number;
  end: number;
  kind: 'video' | 'audio';
}

export const DEFAULT_TEMPLATE = '%(title).150B [%(id)s] clip';

const DEFAULTS: Settings = {
  folder: '',
  quality: 1080,
  template: DEFAULT_TEMPLATE,
  exact: true,
  cookies: '',
  audioFormat: 'mp3',
  concurrent: 2,
  parts: 4,
  ytdlpPath: '',
  ffmpegPath: '',
};

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable: settings last for this session only */
  }
}

@Injectable({ providedIn: 'root' })
export class SettingsService {
  readonly settings = signal<Settings>(read('trimly.settings', DEFAULTS));
  readonly recents = signal<RecentClip[]>(this.loadRecents());

  save(next: Settings) {
    this.settings.set(next);
    write('trimly.settings', next);
  }

  addRecent(clip: RecentClip) {
    const list = [clip, ...this.recents().filter((c) => c.file !== clip.file)].slice(0, 10);
    this.recents.set(list);
    write('trimly.recents', { list });
  }

  clearRecents() {
    this.recents.set([]);
    write('trimly.recents', { list: [] });
  }

  private loadRecents(): RecentClip[] {
    return read<{ list: RecentClip[] }>('trimly.recents', { list: [] }).list;
  }
}
