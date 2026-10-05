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


# 950 is the Windows code page for Big5.  big5hkscs is deliberately absent: it
# extends Big5 with the Hong Kong Supplementary Character Set, which code page
# 950 does not carry, so mapping it here would silently return wrong characters
# rather than a LookupError.
_CODECS = {
    'big5': (950, 'big5'),
    'cp950': (950, 'big5'),
}


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
