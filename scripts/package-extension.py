#!/usr/bin/env python3
"""Package and sign the Chromium extension as CRX3 without storing private keys in the repo."""

from __future__ import annotations

import base64
import hashlib
import os
import shutil
import struct
import subprocess
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXTENSION_DIR = ROOT / "extensions" / "chrome"
CRX_PATH = ROOT / "extensions" / "chrome.crx"
DEFAULT_KEY = Path(os.environ.get("APPDATA", Path.home())) / "RustWallpaper" / "keys" / "chrome-extension.pem"
KEY_PATH = Path(os.environ.get("RUST_WALLPAPER_EXTENSION_KEY", DEFAULT_KEY))


def varint(value: int) -> bytes:
    out = bytearray()
    while value > 0x7F:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def field_bytes(number: int, value: bytes) -> bytes:
    return varint((number << 3) | 2) + varint(len(value)) + value


def main() -> None:
    if not KEY_PATH.is_file():
        raise SystemExit(f"Signing key not found: {KEY_PATH}. Set RUST_WALLPAPER_EXTENSION_KEY to a protected private key path.")

    with tempfile.TemporaryDirectory(prefix="rw-extension-") as temp_dir:
        temp = Path(temp_dir)
        public_der = temp / "public.der"
        subprocess.run(
            ["openssl", "pkey", "-in", str(KEY_PATH), "-pubout", "-outform", "DER", "-out", str(public_der)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        public_key = public_der.read_bytes()
        public_b64 = base64.b64encode(public_key).decode("ascii")

        import json

        manifest_path = EXTENSION_DIR / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["key"] = public_b64
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

        zip_path = temp / "extension.zip"
        with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as archive:
            for path in sorted(EXTENSION_DIR.rglob("*")):
                if path.is_file() and path.name != "manifest.json":
                    archive.write(path, path.relative_to(EXTENSION_DIR).as_posix())
            archive.writestr("manifest.json", manifest_path.read_bytes())
        zip_bytes = zip_path.read_bytes()

        crx_id = hashlib.sha256(public_key).digest()[:16]
        signed_header_data = field_bytes(1, crx_id)
        signed_data = b"CRX3 SignedData\0" + struct.pack("<I", len(signed_header_data)) + signed_header_data + zip_bytes
        signed_data_path = temp / "signed-data.bin"
        signature_path = temp / "signature.bin"
        signed_data_path.write_bytes(signed_data)
        subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(KEY_PATH), "-out", str(signature_path), str(signed_data_path)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        proof = field_bytes(1, public_key) + field_bytes(2, signature_path.read_bytes())
        crx_header = field_bytes(1, proof) + field_bytes(0x10000, signed_header_data)
        CRX_PATH.write_bytes(b"Cr24" + struct.pack("<II", 3, len(crx_header)) + crx_header + zip_bytes)

        extension_id = "".join(chr(ord("a") + nibble) for byte in crx_id for nibble in (byte >> 4, byte & 0x0F))
        print(f"Wrote {CRX_PATH} ({CRX_PATH.stat().st_size} bytes)")
        print(f"Extension ID: {extension_id}")


if __name__ == "__main__":
    main()
