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
        if isinstance(input, str):
            raise TypeError("decoding to str: need a bytes-like object, got str")
        return codecs.code_page_decode(self.code_page, input, errors, final)


# Big5-HKSCS: code page 951 is the proper HKSCS code page but may not be
# available on all Windows builds.  Fall back to Big5 (950) which covers
# the common case (ASCII + Traditional Chinese) even if HKSCS extensions
# are missing.
_CODECS = {
    'big5hkscs': (950, 'big5hkscs'),
}


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
