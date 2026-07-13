#!/bin/bash
set -e

# signal-link.sh — Canonical way to link a Signal account to signal-serve
# Usage: ./signal-link.sh [--store /mnt/usb/signal-serve-data] [--account +1234567890]
#
# 1. Runs signal-serve link on the router (blocks until phone scans)
# 2. Captures provisioning URL from output
# 3. Generates PNG QR code and opens it in Preview
# 4. Waits for phone scan / link to complete
#
# Prerequisites: python3 + qrcode[pil] on Mac

ROUTER="${ROUTER:-root@192.168.8.1}"
STORE="${STORE:-/mnt/usb/signal-serve-data}"

# Parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --store) STORE="$2"; shift 2 ;;
    --router) ROUTER="$2"; shift 2 ;;
    *) echo "Usage: $0 [--store <path>] [--router <user@host>]"; exit 1 ;;
  esac
done

# Ensure qrcode lib available
pip3 install 'qrcode[pil]' -q 2>/dev/null || true

echo "=== Running signal-serve link on $ROUTER ==="
echo "Store: $STORE"
echo ""

# Clean old DB, run link, capture URL
URL=$(ssh -o ConnectTimeout=10 "$ROUTER" "
  rm -f $STORE/signal-serve.db* $STORE/link* $STORE/link-url.txt $STORE/link-qr.html
  /mnt/usb/signal-serve-bin link --store $STORE 2>&1
" | tee /dev/stderr | grep -o 'sgnl://[a-zA-Z0-9?=%&_+/:-]*' | head -1)

if [ -z "$URL" ]; then
  echo ""
  echo "ERROR: No provisioning URL found in output" >&2
  exit 1
fi

echo ""
echo "=== Generating QR ==="
QR_FILE="/tmp/signal-link-$(date +%s).png"

python3 -c "
import qrcode
from PIL import Image
url = '$URL'
qr = qrcode.QRCode(box_size=20, border=4)
qr.add_data(url)
qr.make(fit=True)
img = qr.make_image(fill_color='black', back_color='white')
img.save('$QR_FILE')
print('QR saved: $QR_FILE')
"

# Save URL for reference
echo "$URL" > "${QR_FILE%.png}.txt"
echo "URL: $URL"
echo ""

echo "=== Opening QR in Preview ==="
open "$QR_FILE"

echo ""
echo "Scan QR with Signal mobile > Linked Devices"
echo "Then press ENTER here to verify link status..."
echo ""

# Generate a web version too (easy to reload)
python3 -c "
html = '<html><body><h2>Signal Link QR</h2><img src=\"$QR_FILE\" width=\"400\"/><p><code>$URL</code></p></body></html>'
with open('$QR_FILE.html', 'w') as f:
    f.write(html)
" 2>/dev/null

# Wait for user to confirm
read -r

echo "=== Checking if link succeeded ==="
ssh "$ROUTER" "ls -la $STORE/signal-serve.db 2>/dev/null && echo 'DB exists — account linked!' || echo 'DB not found — link may have failed'"
