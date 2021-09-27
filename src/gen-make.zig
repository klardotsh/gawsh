// Rather than try to fight IFS, let's just handle translating "git rev-list"
// output to Makefile stanzas in a "real" language.

const std = @import("std");

const INPUT_BUF_LEN = 4096;
const OID_STR_LEN = 40;
const FILENAME_STR_LEN = INPUT_BUF_LEN - OID_STR_LEN - 1;

const NamedGitObject = struct {
    oid_str: [OID_STR_LEN]u8,
    filename_str: []const u8,
};

pub fn make_stanza(obj: NamedGitObject) ![]u8 {
    const dirname_fmt = ".gawsh-output/oids/{s}/{s}";
    const target_fmt = "{s}/index.html";
    const rule_fmt =
        \\{s}:
        \\	mkdir -p {s}
        \\	git -C ~/src/Nim cat-file -p {s} | bat -pf --theme ansi --file-name {s} > {s}
    ;
    // -6 removes the two {s} strings
    var dirname_buf: [
        dirname_fmt.len + OID_STR_LEN + FILENAME_STR_LEN
    ]u8 = undefined;
    const dirname = try std.fmt.bufPrint(&dirname_buf, dirname_fmt, .{ obj.oid_str, obj.filename_str });
    var target_buf: [target_fmt.len + dirname_buf.len]u8 = undefined;
    const target = try std.fmt.bufPrint(&target_buf, target_fmt, .{dirname});
    var rule_buf: [rule_fmt.len + target_buf.len]u8 = undefined;
    const rule = try std.fmt.bufPrint(&rule_buf, rule_fmt, .{
        target,
        dirname,
        obj.oid_str,
        obj.filename_str,
        target,
    });

    return rule;
}

pub fn main() !u8 {
    var alloc = std.heap.GeneralPurposeAllocator(.{}){};

    const stdin = std.io.getStdIn().reader();
    const stdout = std.io.getStdOut().writer();

    var buf: [INPUT_BUF_LEN]u8 = undefined;
    var oid: [OID_STR_LEN]u8 = undefined;
    var filename: [FILENAME_STR_LEN]u8 = undefined;

    while (try stdin.readUntilDelimiterOrEof(&buf, '\n')) |oid_name_pair| {
        var idx: usize = 0;
        while (idx < oid.len) {
            oid[idx] = oid_name_pair[idx];
            idx += 1;
        }

        // we know that there's exactly one space between an oid and filename,
        // so skip said space
        idx += 1;

        var fname_len: usize = 0;
        while (idx < oid_name_pair.len) {
            filename[idx - 41] = oid_name_pair[idx];
            idx += 1;
            fname_len += 1;
        }

        const stanza = try make_stanza(NamedGitObject{
            .oid_str = oid,
            .filename_str = std.fs.path.basename(filename[0..fname_len]),
        });

        _ = try stdout.write(try std.fmt.allocPrint(&alloc.allocator, "{s}\n", .{stanza}));
    }

    return 0;
}
