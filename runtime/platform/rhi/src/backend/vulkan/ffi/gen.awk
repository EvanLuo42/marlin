# Generates generated.rs (Vulkan structures, function pointer types and constants) from vk.spec and
# the Vulkan SDK's headers. Needs GNU awk. Run from this directory:
#
#   gawk -f gen.awk vk.spec "$VULKAN_SDK/Include/vulkan/vulkan_core.h" \
#       "$VULKAN_SDK/Include/vulkan/vulkan_win32.h" > generated.rs
#
# vk.spec lines: "struct VkFoo", "fn vkFoo", "const VK_FOO", "type VkFoo" (an alias for a handle,
# enum or flags type nothing else names yet); blank lines and # comments are ignored.
# Every struct and union gets a zeroing `Default` that also fills in `sType`. Types the items refer
# to become aliases (flags u32/u64, enums i32, handles). Bitfield structs and callbacks are
# written by hand in mod.rs.

function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }

function base(t) {
    if (t == "void") return "c_void"
    if (t == "char") return "c_char"
    if (t == "uint8_t") return "u8"
    if (t == "int8_t") return "i8"
    if (t == "uint16_t") return "u16"
    if (t == "int16_t") return "i16"
    if (t == "uint32_t") return "u32"
    if (t == "int32_t" || t == "int") return "i32"
    if (t == "uint64_t") return "u64"
    if (t == "int64_t") return "i64"
    if (t == "float") return "f32"
    if (t == "double") return "f64"
    if (t == "size_t") return "usize"
    if (t ~ /^Vk/ || t ~ /^PFN_/) { used[t] = 1; return t }
    if (t ~ /^(HWND|HINSTANCE|HANDLE|HMONITOR|LPCWSTR)$/) return "*mut c_void"
    if (t == "DWORD") return "u32"
    if (t == "SECURITY_ATTRIBUTES") return "c_void"
    unresolved[t] = 1
    return t
}

# C type text (no name) -> Rust type.
function conv(t,    c1, inner) {
    t = trim(t)
    gsub(/[ \t]+/, " ", t)
    gsub(/ \*/, "*", t)
    if (t ~ /^const .*\* ?const\*$/) {
        inner = t; sub(/^const /, "", inner); sub(/\* ?const\*$/, "", inner)
        return "*const *const " base(trim(inner))
    }
    if (t ~ /\*\*$/) {
        inner = t; sub(/\*\*$/, "", inner); c1 = (inner ~ /^const /); sub(/^const /, "", inner)
        return (c1 ? "*mut *const " : "*mut *mut ") base(trim(inner))
    }
    if (t ~ /\*$/) {
        inner = t; sub(/\*$/, "", inner); c1 = (inner ~ /^const /); sub(/^const /, "", inner)
        return (c1 ? "*const " : "*mut ") base(trim(inner))
    }
    sub(/^const /, "", t)
    sub(/^struct /, "", t)
    return base(t)
}

function rname(n) {
    if (n == "type" || n == "fn" || n == "ref" || n == "match" || n == "loop" || n == "use") return "r#" n
    return n
}

function dim(d) {
    if (d ~ /^[0-9]+$/) return d
    used_const[d] = 1
    return d
}

# "TYPE NAME[N][M]" -> "NAME: RTYPE"
function decl(s, is_param,    name, t, arrs, rt, i, n) {
    s = trim(s)
    n = 0
    while (match(s, /\[[^\]]*\]$/)) {
        arrs[++n] = substr(s, RSTART + 1, RLENGTH - 2)
        s = trim(substr(s, 1, RSTART - 1))
    }
    match(s, /[A-Za-z_][A-Za-z_0-9]*$/)
    name = substr(s, RSTART, RLENGTH)
    t = substr(s, 1, RSTART - 1)
    if (is_param && n > 0) return rname(name) ": *const " conv(t)
    rt = conv(t)
    for (i = n; i >= 1; i--) rt = "[" rt "; " dim(arrs[i]) "]"
    return rname(name) ": " rt
}

function flagtype(t) {
    if (t ~ /FlagBits2/) { sub(/FlagBits2/, "Flags2", t); return t }
    if (t ~ /FlagBits/) { sub(/FlagBits/, "Flags", t); return t }
    return t
}

function cvalue(n, depth,    v) {
    v = cval[n]
    if (v ~ /^VK_/ && depth < 8) return cvalue(v, depth + 1)
    return v
}

function rustval(v) {
    if (v == "(~0U)") return "u32::MAX"
    if (v == "(~0ULL)") return "u64::MAX"
    if (v == "(~1U)") return "!1"
    if (v == "(~2U)") return "!2"
    sub(/ULL$/, "", v); sub(/U$/, "", v); sub(/^\(/, "", v); sub(/\)$/, "", v)
    return v
}

# The VK_STRUCTURE_TYPE_* of struct `n`, matched with underscores removed.
function stype(n,    key) {
    key = toupper(substr(n, 3))
    return (key in stypes) ? stypes[key] : ""
}

FNR == 1 { file++ }

file == 1 {
    line = trim($0)
    if (line == "" || line ~ /^#/) next
    split(line, p, /[ \t]+/)
    want[p[1], p[2]] = 1
    order[++norder] = p[1] " " p[2]
    next
}

{ l = $0 }
capturing {
    if (l ~ /^\}[ \t]*[A-Za-z0-9_]+;/) { capturing = 0; next }
    body[cap] = body[cap] "\n" l
    next
}
l ~ /^typedef (struct|union) Vk[A-Za-z0-9_]+ \{/ {
    match(l, /Vk[A-Za-z0-9_]+/)
    cap = substr(l, RSTART, RLENGTH)
    kind[cap] = (l ~ /^typedef union/) ? "union" : "struct"
    body[cap] = ""
    capturing = 1
    next
}
l ~ /^typedef .*\(VKAPI_PTR \*PFN_vk[A-Za-z0-9_]+\)\(.*\);/ {
    match(l, /\*PFN_vk[A-Za-z0-9_]+\)/)
    f = substr(l, RSTART + 5, RLENGTH - 6)
    ret = l; sub(/^typedef /, "", ret); sub(/ *\(VKAPI_PTR.*/, "", ret)
    args = l; sub(/^[^)]*\)\(/, "", args); sub(/\);[ \t]*$/, "", args)
    pfn_ret[f] = ret
    pfn_args[f] = args
    next
}
l ~ /^typedef VkFlags64 Vk[A-Za-z0-9_]+;/ { split(l, w, /[ ;]+/); flags64[w[3]] = 1; next }
l ~ /^typedef VkFlags Vk[A-Za-z0-9_]+;/ { split(l, w, /[ ;]+/); flags32[w[3]] = 1; next }
l ~ /^VK_DEFINE_HANDLE\(/ { h = l; sub(/^VK_DEFINE_HANDLE\(/, "", h); sub(/\).*/, "", h); dhandle[h] = 1; next }
l ~ /^VK_DEFINE_NON_DISPATCHABLE_HANDLE\(/ { h = l; sub(/^VK_DEFINE_NON_DISPATCHABLE_HANDLE\(/, "", h); sub(/\).*/, "", h); ndhandle[h] = 1; next }
l ~ /^typedef enum Vk[A-Za-z0-9_]+ \{/ { match(l, /Vk[A-Za-z0-9_]+/); cur_enum = substr(l, RSTART, RLENGTH); enums[cur_enum] = 1; next }
cur_enum != "" && l ~ /^\}/ { cur_enum = ""; next }
cur_enum != "" && l ~ /^[ \t]+VK_[A-Z0-9_]+ = / {
    s = trim(l); sub(/,$/, "", s)
    split(s, kv, / = /)
    cval[kv[1]] = kv[2]; ctype[kv[1]] = cur_enum
    if (cur_enum == "VkStructureType" && kv[2] !~ /^VK_/) {
        key = substr(kv[1], 19); gsub(/_/, "", key)
        stypes[key] = kv[1]
    }
    next
}
l ~ /^static const Vk[A-Za-z0-9_]+ VK_[A-Z0-9_]+ = / {
    split(l, w, /[ ;=]+/)
    v = w[5]; sub(/ULL$/, "", v)
    cval[w[4]] = v; ctype[w[4]] = w[3]
    next
}
l ~ /^#define VK_[A-Z0-9_]+ +[(~0-9]/ {
    split(l, w, /[ \t]+/)
    v = w[3]; for (i = 4; i <= NF; i++) v = v w[i]
    cval[w[2]] = v; ctype[w[2]] = "#define"
    next
}
l ~ /^typedef Vk[A-Za-z0-9_]+ Vk[A-Za-z0-9_]+;/ {
    split(l, w, /[ ;]+/)
    talias[w[3]] = w[2]
    next
}

END {
    PROCINFO["sorted_in"] = "@ind_str_asc"
    print "// Generated by gen.awk from vk.spec and the Vulkan SDK headers. Do not edit; add to vk.spec"
    print "// and regenerate."
    print ""
    print "use core::ffi::{c_char, c_void};"
    print ""
    print "use super::{PFN_vkDebugUtilsMessengerCallbackEXT, PFN_vkVoidFunction, VkAllocationCallbacks};"
    for (i = 1; i <= norder; i++) {
        split(order[i], p, " ")
        what = p[1]; n = p[2]
        if (what == "struct") {
            if (!(n in body)) {
                if (n in talias) { print ""; print "pub(crate) type " n " = " talias[n] ";"; used[talias[n]] = 1 }
                else missing[n] = 1
                continue
            }
            if (body[n] ~ /:[0-9]+;/) { bitfield[n] = 1; continue }
            print ""
            print "#[repr(C)]"
            print "#[derive(Clone, Copy)]"
            print "pub(crate) " kind[n] " " n " {"
            m = split(body[n], lines, "\n")
            has_stype = 0
            for (j = 1; j <= m; j++) {
                fl = trim(lines[j]); if (fl == "" || substr(fl, 1, 2) == "//") continue
                sub(/;.*$/, "", fl)
                d = decl(fl, 0)
                if (d ~ /^sType: /) has_stype = 1
                print "    pub " d ","
            }
            print "}"
            print ""
            print "impl Default for " n " {"
            print "    fn default() -> Self {"
            st = has_stype ? stype(n) : ""
            if (st != "") {
                print "        // SAFETY: all-zero is valid for this plain C structure."
                print "        let mut s: Self = unsafe { core::mem::zeroed() };"
                print "        s.sType = " rustval(cvalue(st, 0)) "; // " st
                print "        s"
            } else {
                if (has_stype) nostype[n] = 1
                print "        // SAFETY: all-zero is valid for this plain C structure."
                print "        unsafe { core::mem::zeroed() }"
            }
            print "    }"
            print "}"
            emitted[n] = 1
        } else if (what == "fn") {
            if (!(n in pfn_ret)) { missing[n] = 1; continue }
            a = pfn_args[n]
            out = ""
            if (trim(a) != "void") {
                k = split(a, ps, ",")
                for (j = 1; j <= k; j++) out = out (j > 1 ? ", " : "") decl(ps[j], 1)
            }
            r = trim(pfn_ret[n])
            rs = (r == "void") ? "" : " -> " conv(r)
            fns[++nfns] = "pub(crate) type PFN_" n " = unsafe extern \"system\" fn(" out ")" rs ";"
        } else if (what == "type") {
            used[n] = 1
        } else if (what == "const") {
            if (!(n in cval)) { missing[n] = 1; continue }
            v = cvalue(n, 0); t = ctype[n]
            if (t == "#define") {
                # The type is settled at the end, once array lengths are known.
                consts[++nconsts] = "#define " n " " v
                defined_const[n] = 1
            } else {
                ft = flagtype(t); used[ft] = 1
                consts[++nconsts] = "pub(crate) const " n ": " ft " = " rustval(v) ";"
            }
        }
    }
    print ""
    print "// --- Function pointer types ---"
    print ""
    for (i = 1; i <= nfns; i++) print fns[i]
    print ""
    print "// --- Constants ---"
    print ""
    for (i = 1; i <= nconsts; i++) {
        if (consts[i] ~ /^#define /) {
            split(consts[i], w, " ")
            # Array lengths are usize; ~0ULL values u64; the rest u32.
            if (w[2] in used_const) ty = "usize"
            else if (w[3] ~ /ULL/) ty = "u64"
            else ty = "u32"
            print "pub(crate) const " w[2] ": " ty " = " rustval(w[3]) ";"
        } else print consts[i]
    }
    for (c in used_const) if (!(c in defined_const) && (c in cval))
        print "pub(crate) const " c ": usize = " rustval(cvalue(c, 0)) ";"
    print ""
    print "// --- Referenced types ---"
    print ""
    changed = 1
    while (changed) {
        changed = 0
        for (t in used) if (!(t in done)) { done[t] = 1; changed = 1 }
        for (t in used) if ((t in talias) && !(talias[t] in used)) { used[talias[t]] = 1; changed = 1 }
    }
    for (t in used) {
        if (t in emitted) continue
        if (t in flags64) print "pub(crate) type " t " = u64;"
        else if (t in flags32) print "pub(crate) type " t " = u32;"
        else if (t in dhandle) print "pub(crate) type " t " = *mut c_void;"
        else if (t in ndhandle) print "pub(crate) type " t " = u64;"
        else if (t in enums) print "pub(crate) type " t " = i32;"
        else if (t in talias) print "pub(crate) type " t " = " talias[t] ";"
        else if (t ~ /^PFN_/ || t == "VkAllocationCallbacks") { }
        else if (t == "VkBool32" || t == "VkSampleMask" || t == "VkFlags") print "pub(crate) type " t " = u32;"
        else if (t == "VkDeviceSize" || t == "VkDeviceAddress" || t == "VkFlags64") print "pub(crate) type " t " = u64;"
        else if (t == "VkRemoteAddressNV") print "pub(crate) type VkRemoteAddressNV = *mut c_void;"
        else unresolved[t] = 1
    }
    for (n in missing) print "gen.awk: not in the headers: " n > "/dev/stderr"
    for (n in bitfield) print "gen.awk: bitfields, write by hand: " n > "/dev/stderr"
    for (n in unresolved) print "gen.awk: unresolved type: " n > "/dev/stderr"
    for (n in nostype) print "gen.awk: no sType value found: " n > "/dev/stderr"
}
