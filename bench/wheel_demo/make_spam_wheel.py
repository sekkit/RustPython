"""Assemble spam-1.0-rustpython314-cp314-win_amd64.whl.

Run with RustPython after compiling the extension (see bench/labs/wheel).
Generates a RECORD with correct sha256/size entries so pip installs cleanly.

The wheel tag's interpreter part is "rustpython314" (packaging.tags derives
it from sys.implementation.name) while the ABI part stays CPython-compatible
("cp314", from sysconfig SOABI); the extension file itself keeps the SOABI
suffix (spam.cp314-win_amd64.pyd), exactly like CPython wheels.
"""
import base64
import hashlib
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
TAG = "rustpython314-cp314-win_amd64"
NAME = "spam"
VERSION = "1.0"
PYD = NAME + ".cp314-win_amd64.pyd"
DIST_INFO = NAME + "-" + VERSION + ".dist-info"
WHEEL = NAME + "-" + VERSION + "-" + TAG + ".whl"

METADATA = (
    "Metadata-Version: 2.1\n"
    "Name: %s\n"
    "Version: %s\n"
    "Summary: minimal C extension wheel for RustPython\n"
) % (NAME, VERSION)
WHEEL_META = (
    "Wheel-Version: 1.0\n"
    "Generator: rustpython-lab\n"
    "Root-Is-Purelib: false\n"
    "Tag: %s\n"
) % TAG

def digest_bytes(data):
    h = base64.urlsafe_b64encode(hashlib.sha256(data).digest())
    return h.rstrip(b"=").decode()

records = []
with zipfile.ZipFile(os.path.join(HERE, WHEEL), "w", zipfile.ZIP_DEFLATED) as z:
    for path, data in [
        (PYD, open(os.path.join(HERE, PYD), "rb").read()),
        (DIST_INFO + "/METADATA", METADATA.encode()),
        (DIST_INFO + "/WHEEL", WHEEL_META.encode()),
    ]:
        z.writestr(path, data)
        records.append("%s,sha256=%s,%d" % (path, digest_bytes(data), len(data)))
    records.append(DIST_INFO + "/RECORD,,")
    z.writestr(DIST_INFO + "/RECORD", "\n".join(records) + "\n")

print("wrote " + WHEEL)
