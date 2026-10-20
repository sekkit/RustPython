import codecs
import sys

if sys.platform != 'win32':
    raise ImportError('CJK codecs require the Windows code-page backend')


class _Codec:
    def __init__(self, code_page, name):
        self.code_page = code_page
        self.name = name

    def encode(self, input, errors='strict'):
        return codecs.code_page_encode(self.code_page, input, errors)

    def decode(self, input, errors='strict', final=True):
        return codecs.code_page_decode(self.code_page, input, errors, final)


_CODECS = {
    'gb2312': (936, 'gb2312'),
    'gbk': (936, 'gbk'),
    'gb18030': (936, 'gb18030'),
}


# GB18030's four-byte sequences are a linear numbering of the code points
# Windows cp936 does not cover: idx = (b1-0x81)*12600 + (b2-0x30)*1260 +
# (b3-0x81)*10 + (b4-0x30).  The standard linearizes the uncovered BMP
# ranges (extracted from CPython's codec behaviour) and the supplementary
# planes contiguously from idx 189000 (U+10000).
_GB18030_4B_RANGES = (
    (0x0080, 0x00A3, 0),
    (0x00A5, 0x00A6, 36),
    (0x00A9, 0x00AF, 38),
    (0x00B2, 0x00B6, 45),
    (0x00B8, 0x00D6, 50),
    (0x00D8, 0x00DF, 81),
    (0x00E2, 0x00E7, 89),
    (0x00EB, 0x00EB, 95),
    (0x00EE, 0x00F1, 96),
    (0x00F4, 0x00F6, 100),
    (0x00F8, 0x00F8, 103),
    (0x00FB, 0x00FB, 104),
    (0x00FD, 0x0100, 105),
    (0x0102, 0x0112, 109),
    (0x0114, 0x011A, 126),
    (0x011C, 0x012A, 133),
    (0x012C, 0x0143, 148),
    (0x0145, 0x0147, 172),
    (0x0149, 0x014C, 175),
    (0x014E, 0x016A, 179),
    (0x016C, 0x01CD, 208),
    (0x01CF, 0x01CF, 306),
    (0x01D1, 0x01D1, 307),
    (0x01D3, 0x01D3, 308),
    (0x01D5, 0x01D5, 309),
    (0x01D7, 0x01D7, 310),
    (0x01D9, 0x01D9, 311),
    (0x01DB, 0x01DB, 312),
    (0x01DD, 0x01F8, 313),
    (0x01FA, 0x0250, 341),
    (0x0252, 0x0260, 428),
    (0x0262, 0x02C6, 443),
    (0x02C8, 0x02C8, 544),
    (0x02CC, 0x02D8, 545),
    (0x02DA, 0x0390, 558),
    (0x03A2, 0x03A2, 741),
    (0x03AA, 0x03B0, 742),
    (0x03C2, 0x03C2, 749),
    (0x03CA, 0x0400, 750),
    (0x0402, 0x040F, 805),
    (0x0450, 0x0450, 819),
    (0x0452, 0x200F, 820),
    (0x2011, 0x2012, 7922),
    (0x2017, 0x2017, 7924),
    (0x201A, 0x201B, 7925),
    (0x201E, 0x2024, 7927),
    (0x2027, 0x202F, 7934),
    (0x2031, 0x2031, 7943),
    (0x2034, 0x2034, 7944),
    (0x2036, 0x203A, 7945),
    (0x203C, 0x20AB, 7950),
    (0x20AD, 0x2102, 8062),
    (0x2104, 0x2104, 8148),
    (0x2106, 0x2108, 8149),
    (0x210A, 0x2115, 8152),
    (0x2117, 0x2120, 8164),
    (0x2122, 0x215F, 8174),
    (0x216C, 0x216F, 8236),
    (0x217A, 0x218F, 8240),
    (0x2194, 0x2195, 8262),
    (0x219A, 0x2207, 8264),
    (0x2209, 0x220E, 8374),
    (0x2210, 0x2210, 8380),
    (0x2212, 0x2214, 8381),
    (0x2216, 0x2219, 8384),
    (0x221B, 0x221C, 8388),
    (0x2221, 0x2222, 8390),
    (0x2224, 0x2224, 8392),
    (0x2226, 0x2226, 8393),
    (0x222C, 0x222D, 8394),
    (0x222F, 0x2233, 8396),
    (0x2238, 0x223C, 8401),
    (0x223E, 0x2247, 8406),
    (0x2249, 0x224B, 8416),
    (0x224D, 0x2251, 8419),
    (0x2253, 0x225F, 8424),
    (0x2262, 0x2263, 8437),
    (0x2268, 0x226D, 8439),
    (0x2270, 0x2294, 8445),
    (0x2296, 0x2298, 8482),
    (0x229A, 0x22A4, 8485),
    (0x22A6, 0x22BE, 8496),
    (0x22C0, 0x2311, 8521),
    (0x2313, 0x245F, 8603),
    (0x246A, 0x2473, 8936),
    (0x249C, 0x24FF, 8946),
    (0x254C, 0x254F, 9046),
    (0x2574, 0x2580, 9050),
    (0x2590, 0x2592, 9063),
    (0x2596, 0x259F, 9066),
    (0x25A2, 0x25B1, 9076),
    (0x25B4, 0x25BB, 9092),
    (0x25BE, 0x25C5, 9100),
    (0x25C8, 0x25CA, 9108),
    (0x25CC, 0x25CD, 9111),
    (0x25D0, 0x25E1, 9113),
    (0x25E6, 0x2604, 9131),
    (0x2607, 0x2608, 9162),
    (0x260A, 0x263F, 9164),
    (0x2641, 0x2641, 9218),
    (0x2643, 0x2E80, 9219),
    (0x2E82, 0x2E83, 11329),
    (0x2E85, 0x2E87, 11331),
    (0x2E89, 0x2E8A, 11334),
    (0x2E8D, 0x2E96, 11336),
    (0x2E98, 0x2EA6, 11346),
    (0x2EA8, 0x2EA9, 11361),
    (0x2EAB, 0x2EAD, 11363),
    (0x2EAF, 0x2EB2, 11366),
    (0x2EB4, 0x2EB5, 11370),
    (0x2EB8, 0x2EBA, 11372),
    (0x2EBC, 0x2EC9, 11375),
    (0x2ECB, 0x2FEF, 11389),
    (0x2FFC, 0x2FFF, 11682),
    (0x3004, 0x3004, 11686),
    (0x3018, 0x301C, 11687),
    (0x301F, 0x3020, 11692),
    (0x302A, 0x303D, 11694),
    (0x303F, 0x3040, 11714),
    (0x3094, 0x309A, 11716),
    (0x309F, 0x30A0, 11723),
    (0x30F7, 0x30FB, 11725),
    (0x30FF, 0x3104, 11730),
    (0x312A, 0x321F, 11736),
    (0x322A, 0x3230, 11982),
    (0x3232, 0x32A2, 11989),
    (0x32A4, 0x338D, 12102),
    (0x3390, 0x339B, 12336),
    (0x339F, 0x33A0, 12348),
    (0x33A2, 0x33C3, 12350),
    (0x33C5, 0x33CD, 12384),
    (0x33CF, 0x33D0, 12393),
    (0x33D3, 0x33D4, 12395),
    (0x33D6, 0x3446, 12397),
    (0x3448, 0x3472, 12510),
    (0x3474, 0x359D, 12553),
    (0x359F, 0x360D, 12851),
    (0x360F, 0x3619, 12962),
    (0x361B, 0x3917, 12973),
    (0x3919, 0x396D, 13738),
    (0x396F, 0x39CE, 13823),
    (0x39D1, 0x39DE, 13919),
    (0x39E0, 0x3A72, 13933),
    (0x3A74, 0x3B4D, 14080),
    (0x3B4F, 0x3C6D, 14298),
    (0x3C6F, 0x3CDF, 14585),
    (0x3CE1, 0x4055, 14698),
    (0x4057, 0x415E, 15583),
    (0x4160, 0x4336, 15847),
    (0x4338, 0x43AB, 16318),
    (0x43AD, 0x43B0, 16434),
    (0x43B2, 0x43DC, 16438),
    (0x43DE, 0x44D5, 16481),
    (0x44D7, 0x464B, 16729),
    (0x464D, 0x4660, 17102),
    (0x4662, 0x4722, 17122),
    (0x4724, 0x4728, 17315),
    (0x472A, 0x477B, 17320),
    (0x477D, 0x478C, 17402),
    (0x478E, 0x4946, 17418),
    (0x4948, 0x4979, 17859),
    (0x497B, 0x497C, 17909),
    (0x497E, 0x4981, 17911),
    (0x4984, 0x4984, 17915),
    (0x4987, 0x499A, 17916),
    (0x499C, 0x499E, 17936),
    (0x49A0, 0x49B5, 17939),
    (0x49B8, 0x4C76, 17961),
    (0x4C78, 0x4C9E, 18664),
    (0x4CA4, 0x4D12, 18703),
    (0x4D1A, 0x4DAD, 18814),
    (0x4DAF, 0x4DFF, 18962),
    (0x9FA6, 0xD7FF, 19043),
    (0xE76C, 0xE76C, 33469),
    (0xE7C8, 0xE7C8, 33470),
    (0xE7E7, 0xE7F3, 33471),
    (0xE815, 0xE815, 33484),
    (0xE819, 0xE81D, 33485),
    (0xE81F, 0xE825, 33490),
    (0xE827, 0xE82A, 33497),
    (0xE82D, 0xE830, 33501),
    (0xE833, 0xE83A, 33505),
    (0xE83C, 0xE842, 33513),
    (0xE844, 0xE853, 33520),
    (0xE856, 0xE863, 33536),
    (0xE865, 0xF92B, 33550),
    (0xF92D, 0xF978, 37845),
    (0xF97A, 0xF994, 37921),
    (0xF996, 0xF9E6, 37948),
    (0xF9E8, 0xF9F0, 38029),
    (0xF9F2, 0xFA0B, 38038),
    (0xFA10, 0xFA10, 38064),
    (0xFA12, 0xFA12, 38065),
    (0xFA15, 0xFA17, 38066),
    (0xFA19, 0xFA1E, 38069),
    (0xFA22, 0xFA22, 38075),
    (0xFA25, 0xFA26, 38076),
    (0xFA2A, 0xFE2F, 38078),
    (0xFE32, 0xFE32, 39108),
    (0xFE45, 0xFE48, 39109),
    (0xFE53, 0xFE53, 39113),
    (0xFE58, 0xFE58, 39114),
    (0xFE67, 0xFE67, 39115),
    (0xFE6C, 0xFF00, 39116),
    (0xFF5F, 0xFFDF, 39265),
    (0xFFE6, 0xFFFF, 39394),
)
_GB18030_SUPP_START_CP = 0x10000
_GB18030_SUPP_START_IDX = 189000
_GB18030_SUPP_END_IDX = _GB18030_SUPP_START_IDX + (0x10FFFF - 0x10000)


def _gb18030_encode_4byte(cp):
    """Encode a code point as a GB18030 four-byte sequence."""
    if cp >= _GB18030_SUPP_START_CP:
        idx = _GB18030_SUPP_START_IDX + (cp - _GB18030_SUPP_START_CP)
    else:
        lo, hi = 0, len(_GB18030_4B_RANGES) - 1
        idx = None
        while lo <= hi:
            mid = (lo + hi) // 2
            start, end, base = _GB18030_4B_RANGES[mid]
            if cp < start:
                hi = mid - 1
            elif cp > end:
                lo = mid + 1
            else:
                idx = base + (cp - start)
                break
        if idx is None:
            return None
    b1, rem = divmod(idx, 12600)
    b2, rem = divmod(rem, 1260)
    b3, b4 = divmod(rem, 10)
    return bytes((0x81 + b1, 0x30 + b2, 0x81 + b3, 0x30 + b4))


def _gb18030_decode_4byte(b1, b2, b3, b4):
    """Decode a GB18030 four-byte sequence to a code point (or None)."""
    if not (0x81 <= b1 <= 0xFE and 0x30 <= b2 <= 0x39
            and 0x81 <= b3 <= 0xFE and 0x30 <= b4 <= 0x39):
        return None
    idx = ((b1 - 0x81) * 12600 + (b2 - 0x30) * 1260
           + (b3 - 0x81) * 10 + (b4 - 0x30))
    if _GB18030_SUPP_START_IDX <= idx <= _GB18030_SUPP_END_IDX:
        return _GB18030_SUPP_START_CP + (idx - _GB18030_SUPP_START_IDX)
    lo, hi = 0, len(_GB18030_4B_RANGES) - 1
    while lo <= hi:
        mid = (lo + hi) // 2
        start, end, base = _GB18030_4B_RANGES[mid]
        span = end - start + 1
        if idx < base:
            hi = mid - 1
        elif idx >= base + span:
            lo = mid + 1
        else:
            return start + (idx - base)
    return None


class _GB18030Codec:
    """GB18030: two-byte sequences are GBK (Windows cp936); characters
    outside GBK use the standard four-byte linear mapping."""

    name = 'gb18030'

    def encode(self, input, errors='strict'):
        out = bytearray()
        pos = 0
        length = len(input)
        while pos < length:
            ch = input[pos]
            code = ord(ch)
            start = pos
            pos += 1
            if code < 0x80:
                out.append(code)
                continue
            try:
                encoded, _ = codecs.code_page_encode(936, ch, errors)
                out.extend(encoded)
                continue
            except UnicodeEncodeError:
                pass
            four = _gb18030_encode_4byte(code)
            if four is None:
                if errors != 'strict':
                    handler = codecs.lookup_error(errors)
                    exc = UnicodeEncodeError(
                        self.name, input, start, pos, 'illegal character')
                    replacement, new_pos = handler(exc)
                    if new_pos > length:
                        raise IndexError(
                            'position %d from error handler out of bounds'
                            % new_pos)
                    pos = new_pos
                    if isinstance(replacement, str):
                        for rch in replacement:
                            out.extend(self._encode_one(rch, errors, input,
                                                        start))
                    continue
                raise UnicodeEncodeError(
                    self.name, input, start, pos, 'illegal character')
            out.extend(four)
        return bytes(out), length

    def _encode_one(self, ch, errors, input, start):
        code = ord(ch)
        if code < 0x80:
            return bytes([code])
        try:
            encoded, _ = codecs.code_page_encode(936, ch, errors)
            return encoded
        except UnicodeEncodeError:
            pass
        four = _gb18030_encode_4byte(code)
        if four is None:
            raise UnicodeEncodeError(
                self.name, input, start, start + 1, 'illegal character')
        return four

    def decode(self, input, errors='strict', final=True):
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        if not isinstance(input, (bytes, bytearray, memoryview)):
            raise TypeError("decoding to str: need a bytes-like object, got int")
        data = bytes(input)
        out = []
        pos = 0
        n = len(data)
        while pos < n:
            b = data[pos]
            if b < 0x80:
                out.append(chr(b))
                pos += 1
                continue
            if b == 0x80:
                # Euro sign in GBK
                out.append('\u20ac')
                pos += 1
                continue
            if b >= 0x81:
                if pos + 1 >= n:
                    if not final:
                        break
                    raise UnicodeDecodeError(
                        self.name, data, pos, n,
                        'incomplete multibyte sequence')
                b2 = data[pos + 1]
                if 0x30 <= b2 <= 0x39:
                    # four-byte sequence
                    if pos + 3 >= n:
                        if not final:
                            break
                        raise UnicodeDecodeError(
                            self.name, data, pos, n,
                            'incomplete multibyte sequence')
                    cp = _gb18030_decode_4byte(
                        data[pos], data[pos + 1], data[pos + 2],
                        data[pos + 3])
                    if cp is None:
                        if errors == 'replace':
                            out.append('\ufffd')
                        elif errors == 'ignore':
                            pass
                        else:
                            raise UnicodeDecodeError(
                                self.name, data, pos, pos + 4,
                                'illegal multibyte sequence')
                    else:
                        out.append(chr(cp))
                    pos += 4
                    continue
                # two-byte GBK sequence
                try:
                    text, _ = codecs.code_page_decode(
                        936, data[pos:pos + 2], errors, True)
                    out.append(text)
                except UnicodeDecodeError:
                    if errors == 'replace':
                        out.append('\ufffd')
                    elif errors == 'ignore':
                        pass
                    else:
                        raise UnicodeDecodeError(
                            self.name, data, pos, pos + 2,
                            'illegal multibyte sequence') from None
                pos += 2
                continue
            raise UnicodeDecodeError(
                self.name, data, pos, pos + 1, 'invalid start byte')
        return ''.join(out), n


class _HZCodec:
    """The HZ file encoding: a stateful, ASCII-transparent variant of GB2312.

    A double-byte GB2312 sequence in the range 0xA1A1-0xFEFE is written as
    ``~{`` followed by the two bytes each offset by 0x80, which keeps the whole
    stream 7-bit clean apart from those escaped pairs.  ``~`` is the escape
    character and is doubled.  Unlike the plain code-page codecs above this
    cannot be expressed as a single Windows code page, so it is implemented
    here on top of the GB one.
    """

    name = 'hz'

    def __init__(self, code_page=20936):
        self.code_page = code_page

    def encode(self, input, errors='strict'):
        out, in_double = self.encode_chunk(input, errors, False)
        if in_double:
            out += b'~}'
        return out, len(input)

    def encode_chunk(self, input, errors='strict', in_double=False):
        """Encode one chunk, returning ``(bytes, in_double)``.

        A double-byte run may span chunks, so the caller passes in and gets back
        whether the chunk ended inside a ``~{`` run.
        """
        out = bytearray()
        pos = 0
        length = len(input)

        while pos < length:
            char = input[pos]
            code = ord(char)
            start = pos
            pos += 1
            if code < 0x80:
                if code == 0x7E:                # '~' is the escape character
                    if in_double:
                        out.extend(b'~}')
                        in_double = False
                    out.extend(b'~~')
                    continue
                if in_double:
                    out.extend(b'~}')
                    in_double = False
                out.append(code)
                continue
            if 0xD800 <= code <= 0xDBFF and pos < length:
                low = ord(input[pos])
                if not 0xDC00 <= low <= 0xDFFF:
                    raise UnicodeEncodeError('hz', input, start, pos,
                                             'illegal multibyte sequence')
                pos += 1
                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                char = chr(code)
            try:
                encoded, _ = codecs.code_page_encode(self.code_page, char, errors)
            except UnicodeEncodeError:
                if errors != 'strict':
                    # Custom handler already processed — skip
                    pos = pos + 1
                    continue
                raise UnicodeEncodeError('hz', input, start, pos,
                                         'illegal multibyte sequence') from None
            if not encoded:
                # Character was skipped by error handler
                continue
            if len(encoded) != 2:
                raise UnicodeEncodeError('hz', input, start, pos,
                                         'illegal multibyte sequence')
            if not in_double:
                out.extend(b'~{')
                in_double = True
            # GB2312 bytes are 0xA1-0xFE; the escaped form subtracts 0x80 so
            # the whole stream stays 7-bit clean.
            out.extend(bytes(byte - 0x80 for byte in encoded))
        return bytes(out), in_double

    def decode(self, input, errors='strict', final=True):
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        text, _, _ = self.decode_chunk(input, errors, False, final)
        return text, len(input)

    def decode_chunk(self, input, errors='strict', in_double=False,
                     final=True):
        """Decode one chunk, returning ``(text, consumed)``.

        ``in_double`` carries the ``~{`` run state across incremental
        calls; unconsumed bytes (partial escapes or half a pair) are left
        for the caller to buffer.
        """
        out = []
        pos = 0
        length = len(input)
        in_double = False
        while pos < length:
            byte = input[pos]
            if in_double:
                # Inside a '~{' run every two bytes are one GB2312 character.
                if byte == 0x7E:            # '~' ends the run
                    if pos + 1 >= length:
                        if final:
                            raise UnicodeDecodeError('hz', input, pos, length,
                                                     'truncated \\~ escape sequence')
                        break
                    if input[pos + 1] == 0x7E:
                        out.append('~')
                        pos += 2
                        continue
                    if input[pos + 1] == 0x7D:      # '~}' closes the run
                        in_double = False
                        pos += 2
                        continue
                    raise UnicodeDecodeError('hz', input, pos, pos + 2,
                                             'unknown escape sequence')
                if pos + 1 >= length:
                    if final:
                        raise UnicodeDecodeError('hz', input, pos, length,
                                                 'truncated multibyte sequence')
                    break
                pair = bytes((byte + 0x80, input[pos + 1] + 0x80))
                try:
                    text, _ = codecs.code_page_decode(self.code_page, pair, errors)
                except UnicodeDecodeError:
                    raise UnicodeDecodeError('hz', input, pos, pos + 2,
                                             'invalid multibyte sequence') from None
                out.append(text)
                pos += 2
                continue
            if byte != 0x7E:
                if byte >= 0x80:
                    raise UnicodeDecodeError('hz', input, pos, pos + 1,
                                             'invalid multibyte sequence')
                out.append(chr(byte))
                pos += 1
                continue
            if pos + 1 >= length:
                if final:
                    raise UnicodeDecodeError('hz', input, pos, length,
                                             'truncated \\~ escape sequence')
                break
            nxt = input[pos + 1]
            if nxt == 0x7E:                     # '~~' is a literal '~'
                out.append('~')
                pos += 2
            elif nxt == 0x7D:                   # '~}' closes a double-byte run
                pos += 2
            elif nxt == 0x7B:                   # '~{' opens an escaped run
                if pos + 2 >= length:
                    # A bare '~{' with nothing left is simply not started yet.
                    break
                in_double = True
                pos += 2
            else:
                raise UnicodeDecodeError('hz', input, pos, pos + 2,
                                         'unknown escape sequence')
        # Report only what was actually consumed so an incremental decoder
        # keeps an incomplete escape sequence buffered for the next chunk.
        return ''.join(out), pos, in_double


class IncrementalEncoder(codecs.IncrementalEncoder):
    """Stateful HZ encoder: a ``~{`` run may continue across encode() calls.

    ``encode`` returns just the new bytes, matching
    ``_multibytecodec.MultibyteIncrementalEncoder``.
    """

    def __init__(self, errors='strict'):
        super().__init__(errors)
        self._in_double = False

    def encode(self, input, final=False):
        out, self._in_double = _HZ.encode_chunk(input, self.errors,
                                                self._in_double)
        if final and self._in_double:
            self._in_double = False
            out += b'~}'
        return out


class IncrementalDecoder(codecs.BufferedIncrementalDecoder):
    """Stateful HZ decoder: keeps the ``~{`` run mode across calls.

    ``_buffer_decode`` feeds the accumulated buffer to ``decode_chunk``
    and returns only what was fully consumed, so partial escapes and
    half pairs stay in the base-class buffer.
    """

    def __init__(self, errors='strict'):
        super().__init__(errors)
        self._in_double = False

    def _buffer_decode(self, input, errors, final):
        text, consumed, in_double = _HZ.decode_chunk(
            input, errors, self._in_double, final)
        self._in_double = in_double
        return text, consumed

    def reset(self):
        super().reset()
        self._in_double = False


_HZ = _HZCodec()


def getcodec(name):
    if name == 'hz':
        return _HZ
    if name == 'gb18030':
        return _GB18030Codec()
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
