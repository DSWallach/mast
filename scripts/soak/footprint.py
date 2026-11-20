#!/usr/bin/env python3
"""Print a process's physical footprint in MB (proc_pid_rusage, RUSAGE_INFO_V2). Used by perf-soak.sh."""
import ctypes, sys
class RU(ctypes.Structure):
    _fields_=[("uuid",ctypes.c_uint8*16)]+[(n,ctypes.c_uint64) for n in ("user_time","system_time","pkg_idle_wkups","interrupt_wkups","pageins","wired_size","resident_size","phys_footprint","proc_start_abstime","proc_exit_abstime","child_user_time","child_system_time","child_pkg_idle_wkups","child_interrupt_wkups","child_pageins","child_elapsed_abstime","diskio_bytesread","diskio_byteswritten")]
lib=ctypes.CDLL("/usr/lib/libproc.dylib"); ru=RU()
if lib.proc_pid_rusage(int(sys.argv[1]),2,ctypes.byref(ru))!=0: sys.exit(1)
print(ru.phys_footprint//1048576)
