import codecs


class MultibyteIncrementalEncoder(codecs.IncrementalEncoder):
    codec = None

    def encode(self, input, final=False):
        return self.codec.encode(input, self.errors)[0]


class MultibyteIncrementalDecoder(codecs.BufferedIncrementalDecoder):
    codec = None

    def _buffer_decode(self, input, errors, final):
        return self.codec.decode(input, errors, final)


class MultibyteStreamReader(codecs.StreamReader):
    codec = None

    def decode(self, input, errors='strict'):
        return self.codec.decode(input, errors, True)

    def read(self, size=-1, chars=-1, firstline=False):
        return super().read(-1 if size is None else size, chars, firstline)

    def readline(self, size=None, keepends=True):
        return super().readline(-1 if size is None else size, keepends)

    def readlines(self, sizehint=None, keepends=True):
        return super().readlines(-1 if sizehint is None else sizehint, keepends)


class MultibyteStreamWriter(codecs.StreamWriter):
    codec = None

    def encode(self, input, errors='strict'):
        return self.codec.encode(input, errors)
