using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;

[assembly: AssemblyTitle("MirageSSD Setup")]
[assembly: AssemblyDescription("MirageSSD Windows friend preview installer")]
[assembly: AssemblyCompany("MirageSSD")]
[assembly: AssemblyVersion("0.1.0.0")]
internal static class FriendSetup
{
    [STAThread]
    private static int Main(string[] args)
    {
        if (args.Length == 2 && args[0] == "--verify-only")
        {
            try
            {
                RunPayload(true, delegate(string value) { });
                File.WriteAllText(Path.GetFullPath(args[1]), "PASS: embedded package integrity, desktop configuration, payload startup and prerequisites. Installation and Google sign-in were not run.\r\n");
                return 0;
            }
            catch (Exception error)
            {
                File.WriteAllText(Path.GetFullPath(args[1]), "FAIL: " + error.Message);
                return 1;
            }
        }
        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.Run(new SetupWindow());
        return 0;
    }

    internal static void RunPayload(bool verifyOnly, Action<string> progress)
    {
        string staging = Path.Combine(Path.GetTempPath(), "MirageSSD-Setup-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(staging);
        var security = new DirectorySecurity();
        security.SetAccessRuleProtection(true, false);
        foreach (SecurityIdentifier sid in new[] { WindowsIdentity.GetCurrent().User, new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null), new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null) })
            security.AddAccessRule(new FileSystemAccessRule(sid, FileSystemRights.FullControl, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
        new DirectoryInfo(staging).SetAccessControl(security);
        var extractedFiles = new System.Collections.Generic.List<string>();
        var extractedDirectories = new System.Collections.Generic.List<string>();
        try
        {
            progress("Preparing and checking the installer...");
            string prefix = Path.GetFullPath(staging) + Path.DirectorySeparatorChar;
            string setupScript = null;
            using (Stream payload = Assembly.GetExecutingAssembly().GetManifestResourceStream("MirageSSD.Payload.zip"))
            using (var archive = new ZipArchive(payload, ZipArchiveMode.Read))
            {
                foreach (ZipArchiveEntry entry in archive.Entries)
                {
                    string relative = entry.FullName.Replace('/', Path.DirectorySeparatorChar);
                    if (Path.IsPathRooted(relative)) throw new IOException("Unsafe package path.");
                    string target = Path.GetFullPath(Path.Combine(staging, relative));
                    if (!target.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) throw new IOException("Unsafe package path.");
                    if (String.IsNullOrEmpty(entry.Name)) { Directory.CreateDirectory(target); extractedDirectories.Add(target); continue; }
                    Directory.CreateDirectory(Path.GetDirectoryName(target));
                    extractedDirectories.Add(Path.GetDirectoryName(target));
                    using (Stream source = entry.Open())
                    using (var destination = new FileStream(target, FileMode.CreateNew, FileAccess.Write)) source.CopyTo(destination);
                    extractedFiles.Add(target);
                    if (entry.Name == "setup-miragessd.ps1")
                    {
                        if (setupScript != null) throw new IOException("Ambiguous installer payload.");
                        setupScript = target;
                    }
                }
            }
            if (setupScript == null) throw new IOException("Installer payload is missing.");
            progress(verifyOnly ? "Verifying package..." : "Checking your PC. Approve the filesystem installer if Windows asks, then sign in through Google in your browser.");
            var start = new ProcessStartInfo(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "WindowsPowerShell\\v1.0\\powershell.exe"));
            start.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + setupScript + "\" -Quiet" + (verifyOnly ? " -VerifyOnly" : "");
            start.WorkingDirectory = Path.GetDirectoryName(setupScript);
            // Do not inherit another host's module paths into Windows PowerShell 5.1.
            start.EnvironmentVariables["PSModulePath"] = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "WindowsPowerShell\\v1.0\\Modules");
            start.UseShellExecute = false;
            start.CreateNoWindow = true;
            start.WindowStyle = ProcessWindowStyle.Hidden;
            start.RedirectStandardOutput = true;
            start.RedirectStandardError = true;
            var output = new StringBuilder();
            var errors = new StringBuilder();
            using (var process = new Process())
            {
                process.StartInfo = start;
                process.OutputDataReceived += delegate(object sender, DataReceivedEventArgs item)
                {
                    if (item.Data == null) return;
                    lock (output) { if (output.Length < 12000) output.AppendLine(item.Data); }
                    if (item.Data.StartsWith("Opening Google")) progress("Your browser is open. Sign in to Google and allow MirageSSD to access its files.");
                };
                process.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs item)
                {
                    if (item.Data != null) lock (errors) { if (errors.Length < 12000) errors.AppendLine(item.Data); }
                };
                process.Start();
                process.BeginOutputReadLine();
                process.BeginErrorReadLine();
                process.WaitForExit();
                if (process.ExitCode != 0)
                {
                    string detail = errors.ToString().Trim();
                    if (String.IsNullOrEmpty(detail)) detail = output.ToString().Trim();
                    if (detail.Length > 3500) detail = detail.Substring(detail.Length - 3500);
                    throw new InvalidOperationException("Setup could not finish. Your existing files were not deleted.\r\n\r\n" + detail);
                }
            }
        }
        finally
        {
            // Remove only this run's individually recorded temporary payload.
            // No installation directory, credential record or cache is touched.
            foreach (string file in extractedFiles) { try { File.Delete(file); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
            extractedDirectories.Sort(delegate(string left, string right) { return right.Length.CompareTo(left.Length); });
            foreach (string directory in extractedDirectories) { try { Directory.Delete(directory, false); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
            try { Directory.Delete(staging, false); } catch (IOException) { } catch (UnauthorizedAccessException) { }
        }
    }
}

internal sealed class SetupWindow : Form
{
    private readonly Label status;
    private readonly Button install;
    private readonly ProgressBar progress;
    private bool busy;
    private bool finished;

    internal SetupWindow()
    {
        Text = "MirageSSD Setup";
        ClientSize = new Size(600, 370);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(247, 249, 252);
        Font = new Font("Segoe UI", 10);
        AutoScaleMode = AutoScaleMode.Dpi;
        Controls.Add(new Label { Text = "MirageSSD", Font = new Font("Segoe UI", 27, FontStyle.Bold), ForeColor = Color.FromArgb(30, 62, 113), Location = new Point(30, 24), AutoSize = true });
        Controls.Add(new Label { Text = "Your Google Drive. In This PC.", Font = new Font("Segoe UI", 14), Location = new Point(33, 85), AutoSize = true });
        Controls.Add(new Label { Text = "Install once, sign in through Google, and your drive appears.\r\nA bounded local cache keeps repeat access fast.\r\nWindows 11 x64. At least 12 GiB free on a local NTFS disk.", Location = new Point(34, 128), Size = new Size(535, 75) });
        status = new Label { Text = "Friend preview: cloud files need internet; cached writes upload in the background. Keep originals until verified.", Location = new Point(34, 215), Size = new Size(532, 60), ForeColor = Color.FromArgb(70, 80, 95) };
        Controls.Add(status);
        progress = new ProgressBar { Location = new Point(34, 280), Size = new Size(532, 5), Style = ProgressBarStyle.Marquee, Visible = false };
        Controls.Add(progress);
        install = new Button { Text = "Install and connect Google Drive", Location = new Point(246, 303), Size = new Size(320, 42), BackColor = Color.FromArgb(35, 87, 174), ForeColor = Color.White, FlatStyle = FlatStyle.Flat };
        install.FlatAppearance.BorderSize = 0;
        install.Click += InstallClicked;
        Controls.Add(install);
        FormClosing += delegate(object sender, FormClosingEventArgs args)
        {
            if (busy) { args.Cancel = true; MessageBox.Show(this, "Setup is still running. Complete or cancel the Google sign-in in your browser, then wait for setup to finish.", "MirageSSD"); }
        };
    }

    private async void InstallClicked(object sender, EventArgs args)
    {
        if (finished) { Close(); return; }
        busy = true;
        install.Enabled = false;
        progress.Visible = true;
        try
        {
            await Task.Run(delegate { FriendSetup.RunPayload(false, UpdateStatus); });
            status.Text = "Ready. MirageSSD is available in This PC and starts when you sign in to Windows. Your drive folder has opened.";
            finished = true;
            install.Text = "Done";
        }
        catch (Exception error)
        {
            status.Text = "Setup needs attention. Check the message, then retry. If Google blocks access, ask the developer to add your Google email as a test user.";
            MessageBox.Show(this, error.Message, "MirageSSD setup", MessageBoxButtons.OK, MessageBoxIcon.Warning);
            install.Text = "Retry setup";
        }
        finally { busy = false; progress.Visible = false; install.Enabled = true; }
    }

    private void UpdateStatus(string message)
    {
        if (!IsDisposed && IsHandleCreated) BeginInvoke((Action)delegate { status.Text = message; });
    }
}
