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
        return (''.join(out), i)


_CODEC_INSTANCES = {}
for _n in _CODECS:
    _CODECS[_n]._Iso2022Decoder = _Iso2022Decoder


def getcodec(name):
    try:
        return _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec: ' + name)
