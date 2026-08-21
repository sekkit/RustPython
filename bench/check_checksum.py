import hashlib

def iterallchars():
    return map(chr, range(0xFFFF + 1))

expected = "9e43ee3929471739680c0e705482b4ae1c4122e4"

# Compute predicates-only checksum
h = hashlib.sha1()
for char in iterallchars():
    s1 = char + "abc"
    s2 = char + "ABC"
    s3 = char + "123"
    data = (
        "01"[char.isalnum()],
        "01"[char.isalpha()],
        "01"[char.isdecimal()],
        "01"[char.isdigit()],
        "01"[char.islower()],
        "01"[char.isnumeric()],
        "01"[char.isspace()],
        "01"[char.istitle()],
        "01"[char.isupper()],
        "01"[s1.isalnum()],
        "01"[s1.isalpha()],
        "01"[s3.isdecimal()],
        "01"[s3.isdigit()],
        "01"[s1.islower()],
        "01"[s3.isnumeric()],
        "01"[(char + " \t").isspace()],
        "01"[s1.istitle()],
        "01"[s2.isupper()],
    )
    h.update("".join(data).encode("utf-8", "surrogatepass"))
pred_hash = h.hexdigest()

# Full checksum
h = hashlib.sha1()
for char in iterallchars():
    s1 = char + "abc"
    s2 = char + "ABC"
    s3 = char + "123"
    data = (
        "01"[char.isalnum()],
        "01"[char.isalpha()],
        "01"[char.isdecimal()],
        "01"[char.isdigit()],
        "01"[char.islower()],
        "01"[char.isnumeric()],
        "01"[char.isspace()],
        "01"[char.istitle()],
        "01"[char.isupper()],
        "01"[s1.isalnum()],
        "01"[s1.isalpha()],
        "01"[s3.isdecimal()],
        "01"[s3.isdigit()],
        "01"[s1.islower()],
        "01"[s3.isnumeric()],
        "01"[(char + " \t").isspace()],
        "01"[s1.istitle()],
        "01"[s2.isupper()],
        char.lower(),
        char.upper(),
        char.title(),
        s1.lower(),
        s2.upper(),
        s1.title(),
    )
    h.update("".join(data).encode("utf-8", "surrogatepass"))
full_hash = h.hexdigest()

print("predicates only:", pred_hash)
print("with case:      ", full_hash)
print("expected:       ", expected)
print("predicates match:", pred_hash == expected)
print("full matches:   ", full_hash == expected)