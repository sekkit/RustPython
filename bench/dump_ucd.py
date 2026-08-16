import sys
import unicodedata

# Dump per-char UCD fields to a file: cp<TAB>category<TAB>bidi<TAB>combining<TAB>
# decimal<TAB>digit<TAB>numeric<TAB>mirrored<TAB>eaw<TAB>decomposition<TAB>name
out = sys.argv[1]
limit = int(sys.argv[2]) if len(sys.argv) > 2 else 0x10FFFF
with open(out, 'w', encoding='ascii', errors='backslashreplace') as f:
    for cp in range(limit + 1):
        ch = chr(cp)
        def fmt(v):
            if v is None:
                return '-'
            if isinstance(v, float):
                return repr(v)
            return str(v)
        dec = fmt(unicodedata.decimal(ch, None))
        dig = fmt(unicodedata.digit(ch, None))
        num = fmt(unicodedata.numeric(ch, None))
        decomp = unicodedata.decomposition(ch) or '-'
        name = unicodedata.name(ch, '-')
        f.write('%X\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' % (
            cp,
            unicodedata.category(ch),
            unicodedata.bidirectional(ch),
            unicodedata.combining(ch),
            dec, dig, num,
            unicodedata.mirrored(ch),
            unicodedata.east_asian_width(ch),
            decomp, name,
        ))
print('done', limit + 1)
