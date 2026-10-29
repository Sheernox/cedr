#!/bin/sh
set -eu
npm run build
rsync -a dist/ "$DEST"
