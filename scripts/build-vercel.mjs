// Builds everything Vercel serves from one project:
//   /      the download website (website/)
//   /app/  the Trimly web app (the Angular build)
//
// Run with `npm run build:vercel`. The output goes to dist/vercel.
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';

const out = 'dist/vercel';
const appDir = 'dist/trimly/browser';

// 1. The web app, built to live under /app/
execFileSync('npx', ['ng', 'build', '--base-href', '/app/'], { stdio: 'inherit' });
// 2. Its settings (which Trimly server to talk to), from TRIMLY_API_URL
execFileSync('node', ['scripts/write-web-config.mjs'], { stdio: 'inherit' });
if (!existsSync(appDir)) {
  console.error(`Can't find ${appDir} after the build.`);
  process.exit(1);
}

// 3. Put both together
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync('website', out, { recursive: true });
cpSync(appDir, `${out}/app`, { recursive: true });

// 4. The website's own settings: where the installers are, and where "Use online" goes.
const appConfig = readFileSync(`${appDir}/config.js`, 'utf8');
const repo = /TRIMLY_REPO = "([^"]+)"/.exec(appConfig)?.[1] ?? 'Joshuapavan/Trimly';
writeFileSync(
  `${out}/config.js`,
  `window.TRIMLY_REPO = ${JSON.stringify(repo)};\nwindow.TRIMLY_WEB_APP = "/app/";\n`,
);
console.log(`Built ${out}: website at /, web app at /app/ (releases: ${repo})`);
