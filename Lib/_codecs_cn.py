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
    # GB18030 has no Windows code page (54936 is not one, so encode raised
    # OSError 87).  CPython maps it onto GBK, so do the same here.
    'gb18030': (936, 'gb18030'),
}


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
                raise UnicodeEncodeError('hz', input, start, pos,
                                         'illegal multibyte sequence') from None
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
        return ''.join(out), pos


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
    """Stateful HZ decoder; ``decode`` reports bytes left unconsumed."""


_HZ = _HZCodec()


def getcodec(name):
    if name == 'hz':
        return _HZ
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
