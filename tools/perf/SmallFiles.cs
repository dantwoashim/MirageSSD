using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Web.Script.Serialization;

// Metadata-heavy workload: many small files in a shallow tree. Every phase is
// timed separately and every byte read back is compared with what was
// written. Only paths this process created are ever renamed or deleted.
//
// Usage: SmallFiles.exe <new-directory> <output.json> [files] [bytes] [dirs]
class SmallFiles {
    static void Phase(string phase) { Console.WriteLine(phase); Console.Out.Flush(); }

    static byte[] Content(int index, int bytes) {
        var data = new byte[bytes];
        new Random(20261006 + index).NextBytes(data);
        Buffer.BlockCopy(BitConverter.GetBytes(index), 0, data, 0, Math.Min(4, bytes));
        return data;
    }

    static bool Same(byte[] left, byte[] right) {
        if (left.Length != right.Length) return false;
        for (int i = 0; i < left.Length; i++) if (left[i] != right[i]) return false;
        return true;
    }

    static int Main(string[] args) {
        var row = new Dictionary<string, object>();
        string root = args[0], output = args[1];
        int files = args.Length > 2 ? int.Parse(args[2]) : 2000;
        int bytes = args.Length > 3 ? int.Parse(args[3]) : 4096;
        int dirs = args.Length > 4 ? int.Parse(args[4]) : 20;
        row["directory"] = root; row["files"] = files; row["file_bytes"] = bytes; row["dirs"] = dirs;
        row["started_utc"] = DateTime.UtcNow.ToString("o");
        var ownedDirs = new List<string>();
        var ownedFiles = new List<string>();
        bool rootCreated = false;
        try {
            if (Directory.Exists(root)) throw new Exception("Benchmark directory already exists");
            Directory.CreateDirectory(root); rootCreated = true;
            var paths = new string[files];
            for (int d = 0; d < dirs; d++) {
                string dir = Path.Combine(root, "d" + d.ToString("D3"));
                ownedDirs.Add(dir);
            }
            for (int i = 0; i < files; i++) paths[i] = Path.Combine(ownedDirs[i % dirs], "f" + i.ToString("D5") + ".bin");

            Phase("create");
            var clock = Stopwatch.StartNew();
            foreach (string dir in ownedDirs) Directory.CreateDirectory(dir);
            for (int i = 0; i < files; i++) {
                byte[] data = Content(i, bytes);
                using (var file = new FileStream(paths[i], FileMode.CreateNew, FileAccess.Write, FileShare.None, 4096)) {
                    file.Write(data, 0, data.Length);
                }
                ownedFiles.Add(paths[i]);
            }
            clock.Stop(); row["create_ms"] = clock.Elapsed.TotalMilliseconds;

            Phase("enumerate");
            clock.Restart();
            long seen = 0, total = 0;
            foreach (var info in new DirectoryInfo(root).EnumerateFiles("*", SearchOption.AllDirectories)) { seen++; total += info.Length; }
            clock.Stop(); row["enumerate_ms"] = clock.Elapsed.TotalMilliseconds;
            if (seen != files || total != (long)files * bytes) throw new Exception("Enumeration saw " + seen + " files / " + total + " bytes");

            Phase("stat");
            clock.Restart();
            for (int i = 0; i < files; i++) {
                var info = new FileInfo(paths[i]);
                if (!info.Exists || info.Length != bytes) throw new Exception("Stat mismatch: " + paths[i]);
            }
            clock.Stop(); row["stat_ms"] = clock.Elapsed.TotalMilliseconds;

            Phase("read and verify");
            clock.Restart();
            for (int i = 0; i < files; i++) {
                if (!Same(File.ReadAllBytes(paths[i]), Content(i, bytes))) throw new Exception("Content mismatch: " + paths[i]);
            }
            clock.Stop(); row["read_verify_ms"] = clock.Elapsed.TotalMilliseconds;

            Phase("rename");
            int renamed = 0;
            clock.Restart();
            for (int i = 0; i < files; i += 10) {
                string target = paths[i] + ".renamed";
                File.Move(paths[i], target);
                ownedFiles[i] = target; paths[i] = target; renamed++;
            }
            clock.Stop(); row["rename_ms"] = clock.Elapsed.TotalMilliseconds; row["renamed"] = renamed;

            Phase("delete");
            clock.Restart();
            for (int i = 0; i < files; i++) { File.Delete(ownedFiles[i]); ownedFiles[i] = null; }
            foreach (string dir in ownedDirs) Directory.Delete(dir, false);
            ownedDirs.Clear();
            Directory.Delete(root, false); rootCreated = false;
            clock.Stop(); row["delete_ms"] = clock.Elapsed.TotalMilliseconds;
            row["verified"] = true;
            row["cleaned"] = true;
        } catch (Exception ex) {
            row["error"] = ex.ToString(); row["verified"] = false;
        } finally {
            // Exact owned paths only. Never enumerate-and-delete.
            try {
                foreach (string file in ownedFiles) if (file != null && File.Exists(file)) File.Delete(file);
                foreach (string dir in ownedDirs) if (Directory.Exists(dir)) Directory.Delete(dir, false);
                if (rootCreated && Directory.Exists(root)) Directory.Delete(root, false);
                if (!row.ContainsKey("cleaned")) row["cleaned"] = true;
            } catch (Exception ex) { row["cleanup_error"] = ex.Message; row["cleaned"] = false; }
            File.WriteAllText(output, new JavaScriptSerializer().Serialize(row));
        }
        return row.ContainsKey("error") ? 1 : 0;
    }
}
