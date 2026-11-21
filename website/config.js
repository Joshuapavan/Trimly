// The GitHub repository whose releases the download buttons use.
// The Vercel build (npm run build:vercel) replaces this with the real repository.
window.TRIMLY_REPO = 'Joshuapavan/Trimly';

// The Trimly web app ("Use online"). It asks for an access key the first time.
// Empty hides the "online" buttons. The Vercel build sets this to "/app/",
// because the web app is served from the same project.
window.TRIMLY_WEB_APP = 'https://trimly-clipy.vercel.app/app/';
