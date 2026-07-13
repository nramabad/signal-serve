#!/usr/bin/env python3
"""Run signal-serve link, capture URL, generate QR PNG."""
import subprocess
import sys
import os

STORE = sys.argv[1] if len(sys.argv) > 1 else "/mnt/usb/signal-serve-data"

# Run link, capture stderr for SIGNAL_LINK_URL
proc = subprocess.Popen(
    ["podman", "run", "-i", "--rm", "--network", "host",
     "-v", f"{STORE}:/data",
     "localhost/signal-serve:arm64",
     "link", "--store", "/data"],
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    bufsize=1,
    text=True
)

url = None
for line in proc.stderr:
    print(line, end='', file=sys.stderr)
    if line.startswith("SIGNAL_LINK_URL="):
        url = line.strip().split("=", 1)[1]

for line in proc.stdout:
    print(line, end='')  # pass through stdout too

proc.wait()

if not url:
    print("ERROR: could not find SIGNAL_LINK_URL in output", file=sys.stderr)
    sys.exit(1)

print(f"\n\nProvisioning URL captured: {url}")

# Try to generate QR using qrencode or python
try:
    # Save URL to file
    with open("/tmp/signal-link-url.txt", "w") as f:
        f.write(url)

    qr_path = f"{STORE}/signal-link-qr.png"
    svg_path = f"{STORE}/signal-link.html"

    # Try qrencode first
    subprocess.run(["qrencode", "-o", qr_path, url], check=True)
    print(f"QR PNG saved: {qr_path}")
except FileNotFoundError:
    # Try python
    try:
        import qrcode
        img = qrcode.make(url)
        img.save(qr_path)
        print(f"QR PNG saved: {qr_path}")
    except ImportError:
        # Try pyqrcode
        try:
            import pyqrcode
            qr = pyqrcode.create(url)
            qr.png(qr_path, scale=6)
            print(f"QR PNG saved: {qr_path}")
        except ImportError:
            # Generate HTML with inline QR API
            html = f"""<!DOCTYPE html>
<html><head><title>Signal Link QR</title></head><body>
<h2>Scan with Signal</h2>
<p>Provisioning URL:</p>
<pre style="word-break:break-all;font-size:12px">{url}</pre>
<p><a href="https://api.qrserver.com/v1/create-qr-code/?size=300x300&data={url.replace('tsdevice:/','tsdevice%3A%2F%2F')}" target="_blank">View QR Code</a></p>
<p>Or open Signal mobile > Linked Devices to scan</p>
</body></html>"""
            with open(svg_path, "w") as f:
                f.write(html)
            print(f"QR HTML saved: {svg_path}")
            print(f"\nOpen: http://192.168.8.1:8080/signal-link.html")
except subprocess.CalledProcessError:
    pass

print(f"\nOr copy URL: {url}")
print("Open on phone browser: https://api.qrserver.com/v1/create-qr-code/?size=300x300&data=" + url.replace('tsdevice:/','tsdevice%3A%2F%2F'))
