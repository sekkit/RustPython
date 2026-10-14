"""ISO-2022-JP family codec module (Windows code-page backend).

Implements ISO-2022-JP, JP-1, JP-2, JP-2004, JP-3, JP-EXT, and
ISO-2022-KR by translating between JIS X 0208 and Shift-JIS (cp932).
"""

import codecs
import sys

if sys.platform != 'win32':
    raise ImportError('CJK codecs require the Windows code-page backend')

ESC = 0x1B
ASCII = 0
KANJI = 1

_ESC_MAP = {
    b'(B': ASCII, b'(J': ASCII, b'(I': ASCII,
    b'$B': KANJI, b'$(D': KANJI, b'$(C': KANJI,
    b'$(O': KANJI, b'$(P': KANJI,
}


def _jis_to_sjis(j1, j2):
    """Convert a JIS X 0208 two-byte sequence to Shift-JIS."""
    if j1 <= 0x5e:
        sj1 = (j1 - 0x21) // 2 + 0x81
    else:
        sj1 = (j1 - 0x5f) // 2 + 0xe0
    odd = (j1 - 0x21) % 2 if j1 <= 0x5e else (j1 - 0x5f) % 2
    if odd:
        sj2 = j2 - 0x21 + 0x9f
    else:
        sj2 = j2 - 0x21 + 0x40
        if sj2 >= 0x7f:
            sj2 += 1
    return sj1, sj2


def _sjis_to_jis(s1, s2):
    """Convert a Shift-JIS two-byte sequence to JIS X 0208."""
    if s1 <= 0x9f:
        base = s1 - 0x81
    else:
        base = s1 - 0xe0 + 0x1f
    if s2 >= 0x9f:
        j1 = base * 2 + 0x22
        j2 = s2 - 0x9f + 0x21
    else:
        j1 = base * 2 + 0x21
        j2 = s2 - 0x40 + 0x21
        if s2 > 0x7f:
            j2 -= 1
    return j1, j2
    return j1, j2


class _Codec:
    def __init__(self, name):
        self.name = name

    def encode(self, input, errors='strict'):
        out = bytearray()
        state = ASCII
        for idx, ch in enumerate(input):
            o = ord(ch)
            if o < 0x80:
                if state != ASCII:
                    out.extend(b'\x1b(B')
                    state = ASCII
                out.append(o)
            else:
                try:
                    r = codecs.code_page_encode(932, ch, errors)
                    sb = r[0] if isinstance(r, tuple) else r
                except Exception:
                    if errors == 'replace':
                        if state != ASCII:
                            out.extend(b'\x1b(B')
                            state = ASCII
                        out.append(0x3f)
                        continue
                    elif errors == 'ignore':
                        continue
                    raise UnicodeEncodeError(
                        self.name, input, idx, idx + 1, 'illegal character')
                if len(sb) == 2:
                    j1, j2 = _sjis_to_jis(sb[0], sb[1])
                    if state != KANJI:
                        out.extend(b'\x1b$B')
                        state = KANJI
                    out.append(j1)
                    out.append(j2)
                elif len(sb) == 1 and sb[0] >= 0xa1:
                    if state != ASCII:
                        out.extend(b'\x1b(B')
                        state = ASCII
                    out.append(sb[0])
                else:
                    if state != ASCII:
                        out.extend(b'\x1b(B')
                        state = ASCII
                    out.append(sb[0])
        if state != ASCII:
            out.extend(b'\x1b(B')
        return (bytes(out), len(input))

    def decode(self, input, errors='strict', final=True):
        data = bytes(input)
        out = []
        state = ASCII
        i = 0
        n = len(data)
        while i < n:
            b = data[i]
            if b == ESC and i + 2 < n:
                seq = bytes(data[i + 1:i + 3])
                if seq in _ESC_MAP:
                    state = _ESC_MAP[seq]
                    i += 3
                    continue
            if b == ESC and i + 3 < n:
                seq = bytes(data[i + 1:i + 4])
                if seq in _ESC_MAP:
                    state = _ESC_MAP[seq]
                    i += 4
                    continue
            if b == ESC:
                i += 1
                continue
            if state == KANJI and i + 1 < n:
                j1, j2 = data[i], data[i + 1]
                try:
                    sj1, sj2 = _jis_to_sjis(j1, j2)
                    r = codecs.code_page_decode(
                        932, bytes([sj1, sj2]), errors, True)
                    out.append(r[0] if isinstance(r, tuple) else r)
                except Exception:
                    if errors == 'replace':
                        out.append('\ufffd')
                    elif errors != 'ignore':
                        raise UnicodeDecodeError(
                            self.name, data, i, i + 2, 'invalid sequence')
                i += 2
            else:
                out.append(chr(b))
                i += 1
        return (''.join(out), n)


_CODECS = {
    'iso2022_jp': _Codec('iso2022_jp'),
    'iso2022_jp_1': _Codec('iso2022_jp_1'),
    'iso2022_jp_2': _Codec('iso2022_jp_2'),
    'iso2022_jp_2004': _Codec('iso2022_jp_2004'),
    'iso2022_jp_3': _Codec('iso2022_jp_3'),
    'iso2022_jp_ext': _Codec('iso2022_jp_ext'),
    'iso2022_kr': _Codec('iso2022_kr'),
}


def getcodec(name):
    try:
        return _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec: ' + name)
