import codecs


class MultibyteIncrementalEncoder(codecs.IncrementalEncoder):
    codec = None

    def __init__(self, errors='strict'):
        super().__init__(errors)
        self._enc = None

    def _get_encoder(self):
        if self._enc is None:
            for attr in ('_StatefulEncoder', '_BufferingEncoder'):
                factory = getattr(self.codec, attr, None)
                if factory is None:
                    continue
                try:
                    self._enc = factory(self.codec.name, self.errors)
                except TypeError:
                    self._enc = factory(self.codec.name)
                break
        return self._enc

    def encode(self, input, final=False):
        enc = self._get_encoder()
        if enc is not None:
            return enc.encode(input, final)[0]
        return self.codec.encode(input, self.errors)[0]

    def getstate(self):
        enc = self._get_encoder()
        if enc is not None and hasattr(enc, 'getstate'):
            return enc.getstate()
        return 0

    def setstate(self, state):
        # CPython validates the state format for EVERY codec, stateless or
        # not: byte 0 is the pending length (max 8) and the pending bytes
        # must decode as UTF-8.
        data = state.to_bytes((state.bit_length() + 8) // 8, 'little')
        data = data.ljust(9, b'\x00')
        pending_len = data[0]
        if pending_len > 8:
            raise UnicodeError('pending buffer too long')
        pending_bytes = bytes(data[1:1 + pending_len])
        if pending_bytes:
            pending_bytes.decode('utf-8')
        enc = self._get_encoder()
        if enc is not None and hasattr(enc, 'setstate'):
            enc.setstate(state)

    def reset(self):
        enc = self._get_encoder()
        if enc is not None and hasattr(enc, 'reset'):
            enc.reset()


class MultibyteIncrementalDecoder(codecs.BufferedIncrementalDecoder):
    codec = None

    def __init__(self, errors='strict'):
        super().__init__(errors)
        self._iso = None

    def _get_iso_decoder(self):
        if self._iso is None:
            factory = getattr(self.codec, '_Iso2022Decoder', None)
            if factory is not None:
                self._iso = factory(self.codec.name, self.errors)
        return self._iso

    def _buffer_decode(self, input, errors, final):
        iso = self._get_iso_decoder()
        if iso is not None:
            # ISO-2022: the charset mode persists across calls; the base
            # class keeps unconsumed bytes (partial escapes, half pairs)
            # in self.buffer and passes the combined data as `input`.
            text, consumed = iso.decode_step(input, final)
            return (text, consumed)
        return self.codec.decode(input, errors, final)

    def reset(self):
        super().reset()
        iso = self._get_iso_decoder()
        if iso is not None:
            iso.mode = 0  # ASCII

    def getstate(self):
        # CPython keeps an additional integer flag alongside the pending
        # bytes; round-trip it (test_state_methods: setstate((b'abc',
        # 123456789)) must read back unchanged).
        return (self.buffer, getattr(self, '_flags', 0))

    def setstate(self, state):
        if not isinstance(state, tuple) or len(state) != 2:
            raise TypeError('state must be a (bytes, int) tuple')
        pending, flags = state
        if not isinstance(pending, (bytes, bytearray)):
            raise TypeError('pending state must be bytes')
        if not isinstance(flags, int):
            raise TypeError('state flags must be an int')
        if len(pending) > 8:
            raise UnicodeDecodeError(
                'multibytecodec', pending, 0, len(pending),
                'pending buffer too long')
        self.buffer = bytes(pending)
        self._flags = flags


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
