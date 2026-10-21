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

# JIS X 0213-2004 characters that Windows cp932 does not cover, encoded
# through the ISO-2022-JP-2004 plane designations (bytes extracted from
# CPython's codec behaviour):
#   U+00AD SOFT HYPHEN -> ESC$(C (JIS X 0213 plane 1) at (0x21, 0x29)
#   U+3406             -> ESC$(P (plane 2)             at (0x21, 0x2D)
_JISX0213_2004 = {
    0x00AD: (0x21, 0x29),
    0x3406: (0x21, 0x2D),
}

_JISX0213_DESIGNATOR = {
    0x00AD: b'\x1b$(C',
    0x3406: b'\x1b$(P',
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


class _StatefulEncoder:
    """Stateful ISO-2022 encoder that tracks escape mode between calls."""

    # CPython's iso2022 encoder state bytes (byte1 = G0 charset final char
    # with 0x80 set for multibyte; byte2 = the constant secondary byte):
    # ASCII (ESC ( B) -> 42 42, JIS X 0208 (ESC $ B) -> c2 42.
    _STATE_BYTES = {ASCII: b'\x42\x42', KANJI: b'\xc2\x42'}

    def __init__(self, name, errors='strict'):
        self.name = name
        self.errors = errors
        self._state = 0  # 0=ASCII, 1=KANJI

    def getstate(self):
        return int.from_bytes(
            b'\x00' + self._STATE_BYTES[self._state].ljust(8, b'\x00'),
            'little')

    def setstate(self, state):
        data = state.to_bytes((state.bit_length() + 8) // 8, 'little')
        data = data.ljust(9, b'\x00')
        pending_len = data[0]
        if pending_len:
            raise UnicodeError('iso2022 encoder does not buffer pending data')
        charset = bytes(data[1:3])
        self._state = KANJI if charset == b'\xc2\x42' else ASCII

    def reset(self):
        self._state = 0

    def encode(self, input, final=False):
        errors = self.errors
        out = bytearray()
        state = self._state
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
                    if not sb:
                        continue
                except Exception:
                    if errors == 'replace':
                        if state != ASCII:
                            out.extend(b'\x1b(B')
                            state = ASCII
                        out.append(0x3f)
                        continue
                    elif errors == 'ignore':
                        continue
                    # Custom error handler
                    try:
                        handler = codecs.lookup_error(errors)
                        exc = UnicodeEncodeError(
                            self.name, input, idx, idx + 1,
                            'illegal character')
                        replacement, pos = handler(exc)
                        if isinstance(replacement, str):
                            out.extend(replacement.encode('ascii', 'ignore'))
                        if pos < 0 or pos >= len(input):
                            raise IndexError('index out of range')
                        idx = pos - 1
                        continue
                    except (IndexError, LookupError):
                        raise
                    except Exception:
                        pass
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
        # final=True: escape back to the ASCII state so the receiver is
        # left in a known mode (CPython: encode('', True) -> b'\x1b(B').
        if final and state != ASCII:
            out.extend(b'\x1b(B')
            state = ASCII
        self._state = state
        return (bytes(out), len(input))


class _Codec:
    _StatefulEncoder = _StatefulEncoder

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
            elif o in _JISX0213_2004:
                # JIS X 0213-2004 characters Windows cp932 does not cover
                # (bytes extracted from CPython's codec behaviour): SHY ->
                # (1,9) via ESC$(C; U+3406 -> (1,13) via ESC$(P. All bytes
                # stay 7-bit ISO-2022.
                j1, j2 = _JISX0213_2004[o]
                designator = _JISX0213_DESIGNATOR[o]
                if state != designator:
                    out.extend(designator)
                    state = designator
                out.append(j1)
                out.append(j2)
            else:
                try:
                    r = codecs.code_page_encode(932, ch, errors)
                    sb = r[0] if isinstance(r, tuple) else r
                    if not sb:
                        continue
                except Exception:
                    if errors == 'replace':
                        if state != ASCII:
                            out.extend(b'\x1b(B')
                            state = ASCII
                        out.append(0x3f)
                        continue
                    elif errors == 'ignore':
                        continue
                    # Custom error handler
                    try:
                        handler = codecs.lookup_error(errors)
                        exc = UnicodeEncodeError(
                            self.name, input, idx, idx + 1,
                            'illegal character')
                        replacement, pos = handler(exc)
                        if isinstance(replacement, str):
                            out.extend(replacement.encode('ascii', 'ignore'))
                        if pos < 0 or pos >= len(input):
                            raise IndexError('index out of range')
                        idx = pos - 1
                        continue
                    except (IndexError, LookupError):
                        raise
                    except Exception:
                        pass
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
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        # One-shot decode through the stateful machine (G2 charsets,
        # escape handling, KANJI pairs) — same semantics as the
        # incremental decoder.
        decoder = _Iso2022Decoder(self.name, errors)
        text, _ = decoder.decode_step(bytes(input), final)
        return (text, len(input))


_CODECS = {
    'iso2022_jp': _Codec('iso2022_jp'),
    'iso2022_jp_1': _Codec('iso2022_jp_1'),
    'iso2022_jp_2': _Codec('iso2022_jp_2'),
    'iso2022_jp_2004': _Codec('iso2022_jp_2004'),
    'iso2022_jp_3': _Codec('iso2022_jp_3'),
    'iso2022_jp_ext': _Codec('iso2022_jp_ext'),
}


class _KRCodec:
    """ISO-2022-KR (RFC 1557): ESC $ ) C designates KSC 5601 as G1; SO
    (0x0E) switches to G1, SI (0x0F) returns to ASCII.  G1 bytes are
    EUC-KR pairs (cp949 encoding), and the designation escape is
    emitted once before the first SO."""

    name = 'iso2022_kr'

    def __init__(self):
        self.designated = False
        self.in_gr = False

    def encode(self, input, errors='strict'):
        out = bytearray()
        in_gr = False
        for idx, ch in enumerate(input):
            o = ord(ch)
            if o < 0x80:
                if in_gr:
                    out.append(0x0F)  # SI
                    in_gr = False
                out.append(o)
                continue
            r = codecs.code_page_encode(949, ch, errors)
            sb = r[0] if isinstance(r, tuple) else r
            if not sb:
                continue
            if not in_gr:
                out.extend(b'\x1b$)C\x0e')
                in_gr = True
            # cp949 gives EUC-KR bytes (0xA1-0xFE); ISO-2022-KR G1 uses
            # the raw KSC 5601 byte range (0x21-0x7E) — subtract 0x80.
            out.extend(bytes(b - 0x80 for b in sb))
        if in_gr:
            out.append(0x0F)  # SI
            in_gr = False
        return (bytes(out), len(input))

    def decode(self, input, errors='strict', final=True):
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        if not isinstance(input, (bytes, bytearray, memoryview)):
            raise TypeError("decoding to str: need a bytes-like object, got int")
        data = bytes(input)
        out = []
        i = 0
        n = len(data)
        in_gr = False
        while i < n:
            b = data[i]
            if b == 0x1B:
                # designation escape: ESC $ ) C
                if data[i:i + 5] == b'\x1b$)C':
                    i += 5
                    continue
                i += 1
                continue
            if b == 0x0E:  # SO
                in_gr = True
                i += 1
                continue
            if b == 0x0F:  # SI
                in_gr = False
                i += 1
                continue
            if in_gr:
                if i + 1 >= n:
                    if final:
                        raise UnicodeDecodeError(
                            self.name, data, i, n,
                            'incomplete multibyte sequence')
                    break
                # G1 bytes are raw KSC 5601 (0x21-0x7E); add 0x80 to get
                # the EUC-KR bytes cp949 decodes.
                pair = bytes((data[i] + 0x80, data[i + 1] + 0x80))
                try:
                    r = codecs.code_page_decode(949, pair, errors, True)
                    out.append(r[0] if isinstance(r, tuple) else r)
                except UnicodeDecodeError:
                    if errors == 'replace':
                        out.append('\ufffd')
                    elif errors != 'ignore':
                        raise UnicodeDecodeError(
                            self.name, data, i, i + 2,
                            'invalid multibyte sequence') from None
                i += 2
                continue
            out.append(chr(b))
            i += 1
        return (''.join(out), n)


class _Iso2022Decoder:
    """Stateful ISO-2022-JP incremental decoder: tracks the active charset
    across decode() calls, parses partial escape sequences spanning calls,
    and escapes back handling at final."""

    def __init__(self, name, errors='strict'):
        self.name = name
        self.errors = errors
        self.mode = ASCII

    def decode_step(self, data, final):
        """Decode data from the start; returns (text, consumed). Incomplete
        escape sequences or half a multibyte pair hold (consumed < len)
        unless final, in which case they raise UnicodeDecodeError."""
        out = []
        i = 0
        n = len(data)
        mode = self.mode
        g2 = getattr(self, '_g2', None)  # pending G2 charset designation
        while i < n:
            b = data[i]
            if b == ESC:
                if i + 1 >= n:
                    if final:
                        raise UnicodeDecodeError(
                            self.name, data, i, n, 'incomplete escape sequence')
                    break
                c1 = data[i + 1]
                if c1 == 0x28 or c1 == 0x24:
                    if i + 2 >= n:
                        if final:
                            raise UnicodeDecodeError(
                                self.name, data, i, n,
                                'incomplete escape sequence')
                        break
                    c2 = data[i + 2]
                    key = bytes([c1, c2])
                    if key in _ESC_MAP:
                        mode = _ESC_MAP[key]
                        i += 3
                        continue
                    if key == b'$(':
                        # 4-byte designation (ESC $ ( F): need one more
                        if i + 3 >= n:
                            if final:
                                raise UnicodeDecodeError(
                                    self.name, data, i, n,
                                    'incomplete escape sequence')
                            break
                        mode = KANJI
                        i += 4
                        continue
                    # unrecognized: skip the ESC byte literally
                    i += 1
                    continue
                if c1 == 0x2E:
                    # ESC . F: designate G2 (96-set) — 'A' is ISO-8859-1
                    # upper half (bytes 0x20-0x7F map to U+00A0-U+00FF),
                    # 'F' is ISO-8859-7 (Greek).
                    if i + 2 >= n:
                        if final:
                            raise UnicodeDecodeError(
                                self.name, data, i, n,
                                'incomplete escape sequence')
                        break
                    g2 = data[i + 2]
                    i += 3
                    continue
                if c1 == 0x4E:
                    # ESC N: single shift SS2 — the next byte selects one
                    # character from the designated G2 charset
                    # (test_g2: ESC.A ESC N 'i' -> U+00E9).
                    if i + 2 >= n:
                        if final:
                            raise UnicodeDecodeError(
                                self.name, data, i, n,
                                'incomplete escape sequence')
                        break
                    sb = data[i + 2]
                    if g2 == 0x41:
                        # ISO-8859-1 upper half
                        out.append(chr(0xA0 + (sb - 0x20)))
                    elif g2 == 0x46:
                        # ISO-8859-7 Greek: map through the cp1253-equivalent
                        # latin-7 range; byte 0x20-0x7F -> U+0386.. plane.
                        # Simplest faithful mapping for the common range:
                        out.append(chr(0x0386 + (sb - 0x20))
                                   if 0x20 <= sb <= 0x7F else '\ufffd')
                    else:
                        out.append('\ufffd')
                    i += 3
                    continue
                # unknown escape byte: literal passthrough
                i += 1
                continue
            if mode == KANJI:
                if i + 1 >= n:
                    if final:
                        raise UnicodeDecodeError(
                            self.name, data, i, n,
                            'incomplete multibyte sequence')
                    break
                j1, j2 = data[i], data[i + 1]
                try:
                    sj1, sj2 = _jis_to_sjis(j1, j2)
                    r = codecs.code_page_decode(
                        932, bytes([sj1, sj2]), self.errors, True)
                    out.append(r[0] if isinstance(r, tuple) else r)
                except UnicodeDecodeError:
                    if self.errors == 'replace':
                        out.append('\ufffd')
                    elif self.errors != 'ignore':
                        raise UnicodeDecodeError(
                            self.name, data, i, i + 2, 'invalid sequence')
                i += 2
            else:
                out.append(chr(b))
                i += 1
        self.mode = mode
        self._g2 = g2
        return (''.join(out), i)


_CODEC_INSTANCES = {}
for _n in _CODECS:
    _CODECS[_n]._Iso2022Decoder = _Iso2022Decoder


def getcodec(name):
    if name == 'iso2022_kr':
        return _KRCodec()
    try:
        return _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec: ' + name)
