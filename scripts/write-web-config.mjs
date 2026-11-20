// Writes the built web app's config.js from the TRIMLY_API_URL environment
// variable (the address of your Trimly server). Run after `ng build`.
import { writeFileSync, existsSync } from 'node:fs';

const dir = 'dist/trimly/browser';
if (!existsSync(dir)) {
  console.error(`Can't find ${dir}. Run "ng build" first.`);
  process.exit(1);
}

const raw = (process.env.TRIMLY_API_URL ?? '').trim().replace(/\/+$/, '');
if (raw && !/^https?:\/\//i.test(raw)) {
  console.error(`TRIMLY_API_URL must start with http:// or https:// (got "${raw}").`);
  process.exit(1);
}
if (!raw) {
  console.warn('TRIMLY_API_URL is not set: the web app will look for the server at its own address, and fall back to demo mode if there is none.');
}

writeFileSync(`${dir}/config.js`, `window.TRIMLY_API = ${JSON.stringify(raw)};\n`);
console.log(`Wrote ${dir}/config.js (server: ${raw || 'same address'})`);
