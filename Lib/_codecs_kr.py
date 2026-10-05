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


# 949 is the Windows code page for EUC-KR (the Unified Hangul Code); 1361 is
# Johab.  Both are supersets of EUC-KR in the direction that matters here.
_CODECS = {
    'euc_kr': (949, 'euc_kr'),
    'cp949': (949, 'euc_kr'),
    'johab': (1361, 'johab'),
}


def getcodec(name):
    try:
        code_page, canonical = _CODECS[name]
    except KeyError:
        raise LookupError('unknown codec')
    return _Codec(code_page, canonical)
