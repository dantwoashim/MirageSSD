// Large-file workload: 128 MiB sequential write+flush, buffered and unbuffered
// sequential read with SHA-256 verification, and 1,000 verified random 4 KiB
// reads. Originally used for the 0.1.17 copy comparison (2026-09-29).
//
// Usage: Bench.exe <new-directory> <output.json>
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Web.Script.Serialization;
using Microsoft.Win32.SafeHandles;

class Bench {
    const int Block = 1024 * 1024, Blocks = 128, RandomOps = 1000;
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateFile(string path, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool ReadFile(SafeFileHandle file, IntPtr data, uint count, out uint done, IntPtr overlapped);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint allocation, uint protect);
    [DllImport("kernel32.dll")]
    static extern bool VirtualFree(IntPtr address, UIntPtr size, uint free);
    static string Digest(HashAlgorithm hash) { return BitConverter.ToString(hash.Hash).Replace("-", ""); }
    static void Tag(byte[] buffer, int block) { Buffer.BlockCopy(BitConverter.GetBytes(block), 0, buffer, 0, 4); }
    static void Phase(string phase) { Console.WriteLine(phase); Console.Out.Flush(); }
    static int Main(string[] args) {
        var row = new Dictionary<string, object>();
        string directory = args[0], output = args[1];
        string path = Path.Combine(directory, "large.bin");
        bool created = false;
        row["directory"] = directory; row["bytes"] = (long)Block * Blocks;
        row["request_bytes"] = Block; row["random_ops"] = RandomOps;
        row["started_utc"] = DateTime.UtcNow.ToString("o");
        try {
            if (Directory.Exists(directory)) throw new Exception("Benchmark directory already exists");
            var buffer = new byte[Block]; new Random(20260929).NextBytes(buffer);
            string expected;
            using (var sha = SHA256.Create()) {
                for (int b=0;b<Blocks;b++) { Tag(buffer,b); sha.TransformBlock(buffer,0,Block,null,0); }
                sha.TransformFinalBlock(new byte[0],0,0); expected=Digest(sha);
            }
            row["expected_sha256"] = expected;
            Directory.CreateDirectory(directory); created=true;
            Phase("write"); var clock = Stopwatch.StartNew();
            using (var file = new FileStream(path,FileMode.CreateNew,FileAccess.Write,FileShare.Read,Block,FileOptions.SequentialScan)) {
                for (int b=0;b<Blocks;b++) { Tag(buffer,b); file.Write(buffer,0,Block); }
                row["write_before_flush_ms"]=clock.Elapsed.TotalMilliseconds;
                file.Flush(true);
            }
            clock.Stop(); row["write_flush_ms"]=clock.Elapsed.TotalMilliseconds;
            Phase("warm sequential read and SHA256");
            using (var sha = SHA256.Create()) {
                clock.Restart(); long total=0;
                using(var file=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.Read,Block,FileOptions.SequentialScan)) {
                    int got; while((got=file.Read(buffer,0,Block))>0) { total+=got; sha.TransformBlock(buffer,0,got,null,0); }
                }
                sha.TransformFinalBlock(new byte[0],0,0); clock.Stop();
                if(total!=(long)Block*Blocks || Digest(sha)!=expected) throw new Exception("Buffered read size/hash mismatch");
                row["buffered_read_hash_ms"]=clock.Elapsed.TotalMilliseconds;
            }
            Phase("unbuffered sequential read and SHA256");
            IntPtr aligned=VirtualAlloc(IntPtr.Zero,(UIntPtr)Block,0x3000,4);
            if(aligned==IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
            try {
                using(var sha=SHA256.Create()) {
                    clock.Restart();
                    using(var handle=CreateFile(path,0x80000000,1,IntPtr.Zero,3,0x28000000,IntPtr.Zero)) {
                        if(handle.IsInvalid) throw new System.ComponentModel.Win32Exception();
                        for(int b=0;b<Blocks;b++) {
                            uint got; if(!ReadFile(handle,aligned,Block,out got,IntPtr.Zero)) throw new System.ComponentModel.Win32Exception();
                            if(got!=Block) throw new Exception("Short unbuffered read");
                            Marshal.Copy(aligned,buffer,0,Block); sha.TransformBlock(buffer,0,Block,null,0);
                        }
                    }
                    sha.TransformFinalBlock(new byte[0],0,0); clock.Stop();
                    if(Digest(sha)!=expected) throw new Exception("Unbuffered read hash mismatch");
                    row["unbuffered_read_hash_ms"]=clock.Elapsed.TotalMilliseconds;
                }
            } finally { VirtualFree(aligned,UIntPtr.Zero,0x8000); }
            Phase("warm random 4KiB reads");
            new Random(20260929).NextBytes(buffer);
            var random=new Random(17); var offsets=new int[RandomOps];
            for(int i=0;i<RandomOps;i++) offsets[i]=random.Next(Blocks*Block/4096)*4096;
            var small=new byte[4096]; clock.Restart();
            using(var file=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.Read,4096,FileOptions.RandomAccess)) {
                foreach(int offset in offsets) {
                    file.Position=offset; int count=0;
                    while(count<small.Length) { int got=file.Read(small,count,small.Length-count); if(got==0) throw new Exception("Short random read"); count+=got; }
                    Tag(buffer,offset/Block);
                    for(int j=0;j<small.Length;j++) if(small[j]!=buffer[offset%Block+j]) throw new Exception("Random read mismatch");
                }
            }
            clock.Stop(); row["random_4k_verified_us"]=clock.Elapsed.TotalMilliseconds*1000/RandomOps;
            row["verified"]=true;
        } catch(Exception ex) { row["error"]=ex.ToString(); row["verified"]=false; }
        finally {
            // Exact owned path only. Never enumerate or delete another file.
            if(created) {
                try { if(File.Exists(path)) File.Delete(path); Directory.Delete(directory,false); row["cleaned"]=true; }
                catch(Exception ex) { row["cleanup_error"]=ex.Message; }
            }
            File.WriteAllText(output,new JavaScriptSerializer().Serialize(row));
        }
        return row.ContainsKey("error") ? 1 : 0;
    }
}
