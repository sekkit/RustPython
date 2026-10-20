import codecs
import sys

if sys.platform != 'win32':
    raise ImportError('CJK codecs require the Windows code-page backend')


class _Codec:
    def __init__(self, code_page, name):
        self.code_page = code_page
        self.name = name
        # euc_kr is strict KS X 1001: double-byte sequences use 0xA1-0xFE
        # for BOTH bytes.  Windows cp949 is the Unified Hangul superset and
        # also accepts lead bytes 0x81-0xA0, so plain cp949 decoding is too
        # lenient for the euc-kr codec name.
        self.strict_ksx1001 = (name == 'euc_kr')

    def encode(self, input, errors='strict'):
        return codecs.code_page_encode(self.code_page, input, errors)

    def decode(self, input, errors='strict', final=True):
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        if not isinstance(input, (bytes, bytearray, memoryview)):
            raise TypeError("decoding to str: need a bytes-like object, got int")
        data = bytes(input)
        if not self.strict_ksx1001:
            return codecs.code_page_decode(self.code_page, data, errors, final)
        return self._decode_ksx1001(data, errors, final)

    def _decode_ksx1001(self, data, errors, final):
        """Decode with KS X 1001 byte validation: ASCII passes through,
        double-byte sequences need 0xA1-0xFE for both bytes; anything
        else runs the error protocol (including custom handlers)."""
        out = []
        pos = 0
        n = len(data)
        while pos < n:
            b = data[pos]
            if b < 0x80:
                out.append(chr(b))
                pos += 1
                continue
            if 0xA1 <= b <= 0xFE:
                if pos + 1 >= n:
                    if not final:
                        return (''.join(out), pos)
                    pos = self._error(data, pos, pos + 1, errors, out,
                                      'incomplete multibyte sequence')
                    continue
                b2 = data[pos + 1]
                if 0xA1 <= b2 <= 0xFE:
                    r = codecs.code_page_decode(
                        self.code_page, data[pos:pos + 2], 'strict', True)
                    out.append(r[0] if isinstance(r, tuple) else r)
                    pos += 2
                    continue
                pos = self._error(data, pos, pos + 2, errors, out,
                                  'illegal multibyte sequence')
                continue
            # 0x80-0xA0 (cp949 extension) and 0xFF are invalid in euc-kr
            pos = self._error(data, pos, pos + 1, errors, out,
                              'illegal multibyte sequence')
        return (''.join(out), n)

    def _error(self, data, start, end, errors, out, reason):
        """Run the error protocol at [start, end); returns the new pos and
        appends the handler's replacement to out."""
        if errors == 'strict':
            raise UnicodeDecodeError(self.name, data, start, end, reason)
        handler = codecs.lookup_error(errors)
        exc = UnicodeDecodeError(self.name, data, start, end, reason)
        replacement, new_pos = handler(exc)
        if new_pos > len(data):
            # CPython validates the handler's restart position against the
            # data length: beyond-range positions raise IndexError
            # (sys.maxsize + 1 in test_errorcallback_longindex).
            raise IndexError(
                'position %d from error handler out of bounds' % new_pos)
        out.append(replacement)
        return new_pos


# 949 is the Windows code page for EUC-KR (the Unified Hangul Code); 1361 is
# Johab.  Both are supersets of EUC-KR in the direction that matters here.
_CODECS = {
    'euc_kr': (949, 'euc_kr'),
    'cp949': (949, 'cp949'),
    'johab': (1361, 'johab'),
}


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
