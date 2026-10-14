import codecs


class MultibyteIncrementalEncoder(codecs.IncrementalEncoder):
    codec = None

    def __init__(self, errors='strict'):
        super().__init__(errors)
        self._enc = None

    def _get_encoder(self):
        if self._enc is None and hasattr(self.codec, '_StatefulEncoder'):
            self._enc = self.codec._StatefulEncoder(self.codec.name)
        return self._enc

    def encode(self, input, final=False):
        enc = self._get_encoder()
        if enc is not None:
            return enc.encode(input, self.errors)[0]
        return self.codec.encode(input, self.errors)[0]

    def getstate(self):
        enc = self._get_encoder()
        if enc is not None:
            return getattr(enc, '_state', 0)
        return 0

    def setstate(self, state):
        enc = self._get_encoder()
        if enc is not None:
            enc._state = state

    def reset(self):
        enc = self._get_encoder()
        if enc is not None:
            enc._state = 0


class MultibyteIncrementalDecoder(codecs.BufferedIncrementalDecoder):
    codec = None

    def _buffer_decode(self, input, errors, final):
        return self.codec.decode(input, errors, final)


class MultibyteStreamReader(codecs.StreamReader):
    codec = None

    def __init__(self, stream, errors='strict'):
        if self.codec is None:
            raise AttributeError('codec')
        super().__init__(stream, errors)

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

    def __init__(self, stream, errors='strict'):
        if self.codec is None:
            raise AttributeError('codec')
        super().__init__(stream, errors)

    def encode(self, input, errors='strict'):
        return self.codec.encode(input, errors)
