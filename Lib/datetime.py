"""Specific date/time and related types.

See https://data.iana.org/time-zones/tz-link.html for
time zone and DST data sources.
"""

try:
    from _datetime import *
except ImportError:
    from _pydatetime import *

# TODO: RUSTPYTHON; Simplified datetime_CAPI capsule so C extensions (numpy
# etc.) can call PyDateTime_IMPORT during module init. The capsule stores a
# PyDateTime_CAPI header; type function pointers are filled by the C-API
# layer for the builtin types.
try:
    import ctypes as _ctypes
    _api = _ctypes.pythonapi
    _api.PyCapsule_New.argtypes = [_ctypes.c_void_p, _ctypes.c_char_p, _ctypes.c_void_p]
    _api.PyCapsule_New.restype = _ctypes.py_object

    # Build a minimal PyDateTime_CAPI struct: type pointers first (5 fields),
    # then the function pointers (kept NULL; the C-API layer tolerates them).
    class _PyDateTime_CAPI(_ctypes.Structure):
        _fields_ = [
            ("DateType", _ctypes.c_void_p),
            ("DateTimeType", _ctypes.c_void_p),
            ("TimeType", _ctypes.c_void_p),
            ("DeltaType", _ctypes.c_void_p),
            ("TZInfoType", _ctypes.c_void_p),
            ("TimeZone_UTC", _ctypes.c_void_p),
            # 10 function pointers
            ("Date_FromDate", _ctypes.c_void_p),
            ("DateTime_FromDateAndTime", _ctypes.c_void_p),
            ("Time_FromTime", _ctypes.c_void_p),
            ("Delta_FromDelta", _ctypes.c_void_p),
            ("TimeZone_FromTimeZone", _ctypes.c_void_p),
            ("DateTime_FromTimestamp", _ctypes.c_void_p),
            ("Date_FromTimestamp", _ctypes.c_void_p),
            ("DateTime_FromDateAndTimeAndFold", _ctypes.c_void_p),
            ("Time_FromTimeAndFold", _ctypes.c_void_p),
        ]

    _capistruct = _PyDateTime_CAPI()
    _capistruct.DateType = id(date) if 'date' in globals() else 0
    _capistruct.DateTimeType = id(datetime) if 'datetime' in globals() else 0
    _capistruct.TimeType = id(time) if 'time' in globals() else 0
    _capistruct.DeltaType = id(timedelta) if 'timedelta' in globals() else 0
    _capistruct.TZInfoType = id(tzinfo) if 'tzinfo' in globals() else 0
    _capistruct.TimeZone_UTC = id(UTC) if 'UTC' in globals() else 0
    _datetime_CAPI_capsule = _api.PyCapsule_New(
        _ctypes.c_void_p(_ctypes.addressof(_capistruct)),
        _ctypes.c_char_p(b"datetime.datetime_CAPI\0"),
        None,
    )
    if _datetime_CAPI_capsule is not None:
        globals()["datetime_CAPI"] = _datetime_CAPI_capsule
except Exception:
    pass

__all__ = ("date", "datetime", "time", "timedelta", "timezone", "tzinfo",
           "MINYEAR", "MAXYEAR", "UTC")
