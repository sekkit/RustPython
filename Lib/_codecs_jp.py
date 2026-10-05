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


# Windows code pages that back the Japanese codecs.  932 is Shift-JIS proper,
# which is what cp932 and the shift_jis family use; 21012 is the 1990 JIS X 0212
# code page and 20932 the plain JIS X 0208 one.
_CODECS = {
    'shift_jis': (932, 'shift_jis'),
    'cp932': (932, 'shift_jis'),
    'jisx0213': (932, 'shift_jis'),
}


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
