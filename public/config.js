// Where the Trimly server lives, for the web version.
// Empty means "the same address this page came from" (e.g. the Docker image,
// which serves both). For Vercel, `npm run build:web` rewrites this file from
// the TRIMLY_API_URL environment variable. The desktop app ignores it.
window.TRIMLY_API = '';
