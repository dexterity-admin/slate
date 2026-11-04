"""Writes dist/update.json and dist/update.json.sig for a release.

Usage: python make_manifest.py <dist-dir> <version> <notes-file>
The signing key (PEM, ECDSA P-256) comes from the UPDATE_SIGNING_KEY
environment variable. The signature is base64 of raw r||s over the exact
manifest bytes, which is what src/update.rs verifies with Windows CNG.
"""

import base64
import hashlib
import json
import os
import sys

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature


def main() -> None:
    dist, version, notes_file = sys.argv[1], sys.argv[2], sys.argv[3]
    exe = open(os.path.join(dist, "slate.exe"), "rb").read()
    with open(notes_file, encoding="utf-8") as f:
        notes = f.read().strip()
    manifest = json.dumps(
        {
            "version": version,
            "size": len(exe),
            "sha256": hashlib.sha256(exe).hexdigest(),
            "notes": notes[:4000],
        },
        indent=2,
    ).encode("utf-8")
    key = serialization.load_pem_private_key(os.environ["UPDATE_SIGNING_KEY"].encode(), password=None)
    der = key.sign(manifest, ec.ECDSA(hashes.SHA256()))
    r, s = decode_dss_signature(der)
    raw = r.to_bytes(32, "big") + s.to_bytes(32, "big")
    key.public_key().verify(der, manifest, ec.ECDSA(hashes.SHA256()))
    with open(os.path.join(dist, "update.json"), "wb") as f:
        f.write(manifest)
    with open(os.path.join(dist, "update.json.sig"), "w", encoding="ascii") as f:
        f.write(base64.b64encode(raw).decode())
    print(manifest.decode())


if __name__ == "__main__":
    main()
