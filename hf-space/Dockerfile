# Runs the Trimly server and web app on a Hugging Face Space.
# It reuses the image that .github/workflows/docker.yml publishes, so nothing
# is compiled here.
FROM ghcr.io/joshuapavan/trimly:latest

# Spaces run as a normal user and only /tmp is writable, so keep everything there.
# 7860 is the port a Space expects.
ENV PORT=7860 \
    TRIMLY_DATA_DIR=/tmp/trimly/clips \
    TMPDIR=/tmp/trimly/tmp \
    HOME=/tmp/home

# Be gentle on a shared free machine.
ENV TRIMLY_MAX_JOBS=2 \
    TRIMLY_MAX_PARTS=3 \
    TRIMLY_MAX_CLIP_SECS=900 \
    TRIMLY_FILE_TTL_MINS=60
