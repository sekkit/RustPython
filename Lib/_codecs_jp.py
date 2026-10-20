import codecs
import sys

if sys.platform != 'win32':
    raise ImportError('CJK codecs require the Windows code-page backend')


class _Codec:
    def __init__(self, code_page, name):
        self.code_page = code_page
        self.name = name
        self._jis2004_decode = (name == 'euc_jis_2004')

    def encode(self, input, errors='strict'):
        return codecs.code_page_encode(self.code_page, input, errors)

    def decode(self, input, errors='strict', final=True):
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        if not isinstance(input, (bytes, bytearray, memoryview)):
            raise TypeError("decoding to str: need a bytes-like object, got int")
        data = bytes(input)
        if self._jis2004_decode:
            # Composition-aware one-shot decode: Windows cp20932 does not
            # know the JIS X 0213 composed sequences this codec emits.
            text, _ = _Jis2004Decoder(self.name, errors).decode_step(
                data, final)
            return (text, len(data))
        return codecs.code_page_decode(self.code_page, data, errors, final)


# Windows code pages that back the Japanese codecs.  932 is Shift-JIS proper,
# which is what cp932 and the shift_jis family use; 21012 is the 1990 JIS X 0212
# code page and 20932 the plain JIS X 0208 one.
_CODECS = {
    'shift_jis': (932, 'shift_jis'),
    'cp932': (932, 'shift_jis'),
    # jisx0213 resolves to euc_jis_2004 in CPython (the JIS X 0213 EUC form).
    'jisx0213': (20932, 'euc_jis_2004'),
    'euc_jp': (20932, 'euc_jp'),
    'shift_jis_2004': (932, 'shift_jis_2004'),
    'euc_jis_2004': (20932, 'euc_jis_2004'),
    'shift_jisx0213': (932, 'shift_jisx0213'),
    'euc_jisx0213': (20932, 'euc_jisx0213'),
}

# --- JIS X 0213 combining-character composition (euc_jis_2004) ---------
# CPython's euc_jis_2004 buffers base characters that may take a
# combining mark, then composes base + mark into the JIS X 0213
# sequence.  The tables below were extracted from CPython's codec
# behaviour: _BUFFERING_BASES are the code points held pending;
# _COMPOSITION maps (base, mark) to the exact EUC byte output (either a
# composed two-byte glyph or the base+mark byte sequence);
# _SINGLE maps code points whose EUC bytes Windows cp20932 does not
# produce (or produces differently from CPython).

_MARKS = frozenset((0x0300, 0x0301, 0x309A))

_BUFFERING_BASES = frozenset((
    0x00E6, 0x0254, 0x0259, 0x025A, 0x028C, 0x02E5, 0x02E9,
    0x304B, 0x304D, 0x304F, 0x3051, 0x3053,
    0x30AB, 0x30AD, 0x30AF, 0x30B1, 0x30B3, 0x30BB, 0x30C4, 0x30C8,
    0x31F7,
))

_SINGLE = {
    0x00E6: b'\xa9\xdc',
    0x0254: b'\xab\xb8',
    0x0259: b'\xab\xb0',
    0x025A: b'\xab\xc3',
    0x028C: b'\xab\xb7',
    0x02E5: b'\xab\xe0',
    0x02E9: b'\xab\xe4',
    0x0300: b'\xab\xdc',
    0x0301: b'\xab\xda',
    0x31F7: b'\xa6\xf5',
}

_COMPOSITION = {
    (0x00E6, 0x0300): b'\xab\xc4',
    (0x00E6, 0x0301): b'\xa9\xdc\xab\xda',
    (0x0254, 0x0300): b'\xab\xc8',
    (0x0254, 0x0301): b'\xab\xc9',
    (0x0259, 0x0300): b'\xab\xcc',
    (0x0259, 0x0301): b'\xab\xcd',
    (0x025A, 0x0300): b'\xab\xce',
    (0x025A, 0x0301): b'\xab\xcf',
    (0x028C, 0x0300): b'\xab\xca',
    (0x028C, 0x0301): b'\xab\xcb',
    (0x02E5, 0x0300): b'\xab\xe0\xab\xdc',
    (0x02E5, 0x0301): b'\xab\xe0\xab\xda',
    (0x02E9, 0x0300): b'\xab\xe4\xab\xdc',
    (0x02E9, 0x0301): b'\xab\xe4\xab\xda',
    (0x304B, 0x0300): b'\xa4\xab\xab\xdc',
    (0x304B, 0x0301): b'\xa4\xab\xab\xda',
    (0x304B, 0x309A): b'\xa4\xf7',
    (0x304D, 0x0300): b'\xa4\xad\xab\xdc',
    (0x304D, 0x0301): b'\xa4\xad\xab\xda',
    (0x304D, 0x309A): b'\xa4\xf8',
    (0x304F, 0x0300): b'\xa4\xaf\xab\xdc',
    (0x304F, 0x0301): b'\xa4\xaf\xab\xda',
    (0x304F, 0x309A): b'\xa4\xf9',
    (0x3051, 0x0300): b'\xa4\xb1\xab\xdc',
    (0x3051, 0x0301): b'\xa4\xb1\xab\xda',
    (0x3051, 0x309A): b'\xa4\xfa',
    (0x3053, 0x0300): b'\xa4\xb3\xab\xdc',
    (0x3053, 0x0301): b'\xa4\xb3\xab\xda',
    (0x3053, 0x309A): b'\xa4\xfb',
    (0x30AB, 0x0300): b'\xa5\xab\xab\xdc',
    (0x30AB, 0x0301): b'\xa5\xab\xab\xda',
    (0x30AB, 0x309A): b'\xa5\xf7',
    (0x30AD, 0x0300): b'\xa5\xad\xab\xdc',
    (0x30AD, 0x0301): b'\xa5\xad\xab\xda',
    (0x30AD, 0x309A): b'\xa5\xf8',
    (0x30AF, 0x0300): b'\xa5\xaf\xab\xdc',
    (0x30AF, 0x0301): b'\xa5\xaf\xab\xda',
    (0x30AF, 0x309A): b'\xa5\xf9',
    (0x30B1, 0x0300): b'\xa5\xb1\xab\xdc',
    (0x30B1, 0x0301): b'\xa5\xb1\xab\xda',
    (0x30B1, 0x309A): b'\xa5\xfa',
    (0x30B3, 0x0300): b'\xa5\xb3\xab\xdc',
    (0x30B3, 0x0301): b'\xa5\xb3\xab\xda',
    (0x30B3, 0x309A): b'\xa5\xfb',
    (0x30BB, 0x0300): b'\xa5\xbb\xab\xdc',
    (0x30BB, 0x0301): b'\xa5\xbb\xab\xda',
    (0x30BB, 0x309A): b'\xa5\xfc',
    (0x30C4, 0x0300): b'\xa5\xc4\xab\xdc',
    (0x30C4, 0x0301): b'\xa5\xc4\xab\xda',
    (0x30C4, 0x309A): b'\xa5\xfd',
    (0x30C8, 0x0300): b'\xa5\xc8\xab\xdc',
    (0x30C8, 0x0301): b'\xa5\xc8\xab\xda',
    (0x30C8, 0x309A): b'\xa5\xfe',
    (0x31F7, 0x0300): b'\xa6\xf5\xab\xdc',
    (0x31F7, 0x0301): b'\xa6\xf5\xab\xda',
    (0x31F7, 0x309A): b'\xa6\xf8',
}


def _build_decode_map():
    """Inverse of the encoder's special mappings for the decode direction:
    composed two-byte sequences map back to base + combining mark; the
    explicit single-character bytes map back to their character (Windows
    cp20932 decodes some of them differently or not at all)."""
    m = {}
    for (base, mark), out in _COMPOSITION.items():
        if len(out) == 2:
            m[out] = chr(base) + chr(mark)
    for cp, out in _SINGLE.items():
        m[out] = chr(cp)
    return m


_DECODE_MAP = _build_decode_map()


def _decode_one_pair(b1, b2):
    """Decode one two-byte euc_jis_2004 pair, preferring the codec's own
    composition/single mappings over Windows cp20932."""
    pair = bytes((b1, b2))
    if pair in _DECODE_MAP:
        return _DECODE_MAP[pair]
    r = codecs.code_page_decode(20932, pair, 'strict', True)
    return r[0] if isinstance(r, tuple) else r


class _Jis2004Decoder:
    """Stateful euc_jis_2004 incremental decoder: two-byte pairs decode
    through the codec's own composition-aware table first."""

    def __init__(self, name, errors='strict'):
        self.name = name
        self.errors = errors

    def decode_step(self, data, final):
        out = []
        i = 0
        n = len(data)
        while i < n:
            b = data[i]
            if b < 0x80:
                out.append(chr(b))
                i += 1
                continue
            if 0xA1 <= b <= 0xFE:
                if i + 1 >= n:
                    if final:
                        raise UnicodeDecodeError(
                            self.name, data, i, n,
                            'incomplete multibyte sequence')
                    break
                b2 = data[i + 1]
                if not (0xA1 <= b2 <= 0xFE):
                    raise UnicodeDecodeError(
                        self.name, data, i, i + 2,
                        'illegal multibyte sequence')
                try:
                    out.append(_decode_one_pair(b, b2))
                except UnicodeDecodeError:
                    if self.errors == 'replace':
                        out.append('\ufffd')
                    elif self.errors != 'ignore':
                        raise UnicodeDecodeError(
                            self.name, data, i, i + 2,
                            'illegal multibyte sequence')
                i += 2
                continue
            # three-byte JIS X 0212 plane (0x8F ss2)
            if b == 0x8F:
                if i + 2 >= n:
                    if final:
                        raise UnicodeDecodeError(
                            self.name, data, i, n,
                            'incomplete multibyte sequence')
                    break
                try:
                    r = codecs.code_page_decode(
                        20932, bytes(data[i:i + 3]), self.errors, True)
                    out.append(r[0] if isinstance(r, tuple) else r)
                except UnicodeDecodeError:
                    if self.errors == 'replace':
                        out.append('\ufffd')
                    elif self.errors != 'ignore':
                        raise UnicodeDecodeError(
                            self.name, data, i, i + 3,
                            'illegal multibyte sequence')
                i += 3
                continue
            raise UnicodeDecodeError(
                self.name, data, i, i + 1, 'illegal multibyte sequence')
        return ''.join(out), i


def _encode_one(ch):
    """Encode a single character to euc_jis_2004 bytes."""
    cp = ord(ch)
    if cp in _SINGLE:
        return _SINGLE[cp]
    r = codecs.code_page_encode(20932, ch, 'strict')
    return r[0] if isinstance(r, tuple) else r


class _BufferingEncoder:
    """Stateful euc_jis_2004 encoder: buffers composable bases, composes
    with combining marks, and reports CPython's integer getstate format
    (byte 0 = pending UTF-8 length, bytes 1..8 = pending UTF-8 bytes)."""

    def __init__(self, name, errors='strict'):
        self.name = name
        self.errors = errors
        self.pending = None  # buffered base code point

    def encode(self, input, final=False):
        out = bytearray()
        for ch in input:
            cp = ord(ch)
            if self.pending is not None:
                if cp in _MARKS and (self.pending, cp) in _COMPOSITION:
                    out += _COMPOSITION[(self.pending, cp)]
                    self.pending = None
                    continue
                if cp not in _MARKS:
                    # Encode the new char FIRST: if it fails the pending
                    # buffer must survive the failed call (CPython keeps
                    # the buffer, test_stateful_keep_buffer).
                    new_bytes = _encode_one(ch)
                    out += _encode_one(chr(self.pending))
                    out += new_bytes
                    self.pending = None
                    continue
                # A mark with no composition entry falls through to the
                # plain encode below (which raises for 309a alone).
            if cp in _BUFFERING_BASES:
                self.pending = cp
            else:
                out += _encode_one(ch)
        if final and self.pending is not None:
            out += _encode_one(chr(self.pending))
            self.pending = None
        return (bytes(out), len(input))

    def getstate(self):
        if self.pending is None:
            pending_bytes = b''
        else:
            pending_bytes = chr(self.pending).encode('utf-8')
        return int.from_bytes(
            bytes([len(pending_bytes)]) + pending_bytes.ljust(8, b'\x00'),
            'little')

    def setstate(self, state):
        data = state.to_bytes((state.bit_length() + 8) // 8, 'little')
        data = data.ljust(9, b'\x00')
        pending_len = data[0]
        if pending_len > 8:
            raise UnicodeError('pending buffer too long')
        pending_bytes = bytes(data[1:1 + pending_len])
        if pending_bytes:
            self.pending = ord(pending_bytes.decode('utf-8'))
        else:
            self.pending = None

    def reset(self):
        self.pending = None


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    codec = _Codec(code_page, canonical)
    if code_page == 20932 and canonical == 'euc_jis_2004':
        codec._BufferingEncoder = _BufferingEncoder
        codec._Jis2004Decoder = _Jis2004Decoder
    return codec
