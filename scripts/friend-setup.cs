using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using System.Windows.Forms;

[assembly: AssemblyTitle("MirageSSD Setup")]
[assembly: AssemblyDescription("MirageSSD Windows friend preview installer")]
[assembly: AssemblyCompany("MirageSSD")]
[assembly: AssemblyVersion("0.1.3.0")]
internal static class FriendSetup
{
    private static string LocalApplicationData
    {
        get { return Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData); }
    }

    internal static string DeviceRoot
    {
        get { return Path.Combine(LocalApplicationData, "MirageSSD", "device"); }
    }

    internal static bool IsInstalled
    {
        get
        {
            return File.Exists(Path.Combine(DeviceRoot, "device-install.json"))
                && File.Exists(Path.Combine(DeviceRoot, "mirage.exe"));
        }
    }

    internal static bool IsSignedIn
    {
        get
        {
            return File.Exists(Path.Combine(LocalApplicationData, "MirageSSD", "credentials", "drive-token.json"));
        }
    }

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

    internal static string SignedInAccount()
    {
        try
        {
            string executable = Path.Combine(DeviceRoot, "mirage.exe");
            if (!File.Exists(executable)) return null;
            var start = new ProcessStartInfo(executable)
            {
                Arguments = "--json backend status",
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true
            };
            using (var process = Process.Start(start))
            {
                var stdout = process.StandardOutput.ReadToEndAsync();
                var stderr = process.StandardError.ReadToEndAsync();
                if (!process.WaitForExit(8000))
                {
                    try { process.Kill(); } catch { }
                    return null;
                }
                if (process.ExitCode != 0) return null;
                string output = stdout.GetAwaiter().GetResult();
                stderr.GetAwaiter().GetResult();
                var match = Regex.Match(output, "\"account_id\"\\s*:\\s*\"([^\"]+)\"");
                return match.Success ? match.Groups[1].Value : null;
            }
        }
        catch
        {
            return null;
        }
    }

    internal static void RunPayload(bool verifyOnly, Action<string> progress)
    {
        progress("Preparing and checking the installer...");
        using (Staging staging = Staging.Extract())
        {
            string setupScript = staging.Script("setup-miragessd.ps1");
            if (setupScript == null) throw new IOException("Installer payload is missing.");
            progress(verifyOnly ? "Verifying package..." : "Checking your PC. Approve the filesystem installer if Windows asks, then sign in through Google in your browser.");
            RunScript(
                setupScript,
                "-Quiet" + (verifyOnly ? " -VerifyOnly" : ""),
                delegate(string line)
                {
                    return line.StartsWith("Opening Google")
                        ? "Your browser is open. Sign in to Google and allow MirageSSD to access its files."
                        : null;
                },
                progress);
        }
    }

    internal static void RunAccountAction(string action, Action<string> progress)
    {
        progress("Preparing MirageSSD account controls...");
        using (Staging staging = Staging.Extract())
        {
            string accountScript = staging.Script("account-device.ps1");
            if (accountScript == null) throw new IOException("Installer payload is missing its account tool.");
            RunScript(
                accountScript,
                "-Quiet -NoConfirm -Action " + action,
                delegate(string line)
                {
                    if (line.Length == 0 || line.Length > 200 || line.StartsWith("{")) return null;
                    return line;
                },
                progress);
        }
    }

    private static void RunScript(string scriptPath, string arguments, Func<string, string> outputMessage, Action<string> progress)
    {
        var start = new ProcessStartInfo(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "WindowsPowerShell\\v1.0\\powershell.exe"));
        start.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + scriptPath + "\" " + arguments;
        start.WorkingDirectory = Path.GetDirectoryName(scriptPath);
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
                string message = outputMessage(item.Data);
                if (message != null) progress(message);
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
                throw new InvalidOperationException("MirageSSD could not finish. Your existing files were not deleted.\r\n\r\n" + detail);
            }
        }
    }

    private sealed class Staging : IDisposable
    {
        private readonly string root;
        private readonly List<string> extractedFiles = new List<string>();
        private readonly List<string> extractedDirectories = new List<string>();
        private readonly Dictionary<string, string> scripts = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        private Staging(string root)
        {
            this.root = root;
        }

        internal string Script(string name)
        {
            string path;
            return scripts.TryGetValue(name, out path) ? path : null;
        }

        internal static Staging Extract()
        {
            string root = Path.Combine(Path.GetTempPath(), "MirageSSD-Setup-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(root);
            var security = new DirectorySecurity();
            security.SetAccessRuleProtection(true, false);
            foreach (SecurityIdentifier sid in new[] { WindowsIdentity.GetCurrent().User, new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null), new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null) })
                security.AddAccessRule(new FileSystemAccessRule(sid, FileSystemRights.FullControl, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
            new DirectoryInfo(root).SetAccessControl(security);
            var staging = new Staging(root);
            try
            {
                string prefix = Path.GetFullPath(root) + Path.DirectorySeparatorChar;
                using (Stream payload = Assembly.GetExecutingAssembly().GetManifestResourceStream("MirageSSD.Payload.zip"))
                using (var archive = new ZipArchive(payload, ZipArchiveMode.Read))
                {
                    foreach (ZipArchiveEntry entry in archive.Entries)
                    {
                        string relative = entry.FullName.Replace('/', Path.DirectorySeparatorChar);
                        if (Path.IsPathRooted(relative)) throw new IOException("Unsafe package path.");
                        string target = Path.GetFullPath(Path.Combine(root, relative));
                        if (!target.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) throw new IOException("Unsafe package path.");
                        if (String.IsNullOrEmpty(entry.Name)) { Directory.CreateDirectory(target); staging.extractedDirectories.Add(target); continue; }
                        Directory.CreateDirectory(Path.GetDirectoryName(target));
                        staging.extractedDirectories.Add(Path.GetDirectoryName(target));
                        using (Stream source = entry.Open())
                        using (var destination = new FileStream(target, FileMode.CreateNew, FileAccess.Write)) source.CopyTo(destination);
                        staging.extractedFiles.Add(target);
                        if (entry.Name.EndsWith(".ps1", StringComparison.OrdinalIgnoreCase))
                        {
                            if (staging.scripts.ContainsKey(entry.Name)) throw new IOException("Ambiguous installer payload.");
                            staging.scripts.Add(entry.Name, target);
                        }
                    }
                }
            }
            catch
            {
                staging.Dispose();
                throw;
            }
            return staging;
        }

        public void Dispose()
        {
            // Remove only this run's individually recorded temporary payload.
            // No installation directory, credential record or cache is touched.
            foreach (string file in extractedFiles) { try { File.Delete(file); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
            extractedDirectories.Sort(delegate(string left, string right) { return right.Length.CompareTo(left.Length); });
            foreach (string directory in extractedDirectories) { try { Directory.Delete(directory, false); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
            try { Directory.Delete(root, false); } catch (IOException) { } catch (UnauthorizedAccessException) { }
        }
    }
}

internal sealed class SetupWindow : Form
{
    private readonly Label status;
    private readonly Button primary;
    private readonly Button secondary;
    private readonly Button reinstall;
    private readonly ProgressBar progress;
    private bool busy;
    private bool finished;
    private bool installed;
    private bool signedIn;

    internal SetupWindow()
    {
        Text = "MirageSSD Setup";
        ClientSize = new Size(600, 410);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(247, 249, 252);
        Font = new Font("Segoe UI", 10);
        AutoScaleMode = AutoScaleMode.Dpi;
        Controls.Add(new Label { Text = "MirageSSD", Font = new Font("Segoe UI", 27, FontStyle.Bold), ForeColor = Color.FromArgb(30, 62, 113), Location = new Point(30, 24), AutoSize = true });
        Controls.Add(new Label { Text = "Your Google Drive. In This PC.", Font = new Font("Segoe UI", 14), Location = new Point(33, 85), AutoSize = true });
        Controls.Add(new Label { Text = "Install once, sign in through Google, and your drive appears.\r\nA bounded local cache keeps repeat access fast.\r\nWindows 11 x64. At least 12 GiB free on a local NTFS disk.", Location = new Point(34, 128), Size = new Size(535, 75) });
        status = new Label { Location = new Point(34, 215), Size = new Size(532, 60), ForeColor = Color.FromArgb(70, 80, 95) };
        Controls.Add(status);
        progress = new ProgressBar { Location = new Point(34, 280), Size = new Size(532, 5), Style = ProgressBarStyle.Marquee, Visible = false };
        Controls.Add(progress);
        secondary = new Button { Location = new Point(34, 303), Size = new Size(200, 42), BackColor = Color.White, ForeColor = Color.FromArgb(60, 70, 85), FlatStyle = FlatStyle.Flat };
        secondary.FlatAppearance.BorderColor = Color.FromArgb(198, 206, 216);
        secondary.Click += SecondaryClicked;
        Controls.Add(secondary);
        primary = new Button { Location = new Point(246, 303), Size = new Size(320, 42), BackColor = Color.FromArgb(35, 87, 174), ForeColor = Color.White, FlatStyle = FlatStyle.Flat };
        primary.FlatAppearance.BorderSize = 0;
        primary.Click += PrimaryClicked;
        Controls.Add(primary);
        FormClosing += delegate(object sender, FormClosingEventArgs args)
        {
            if (busy) { args.Cancel = true; MessageBox.Show(this, "MirageSSD is still working. Complete or cancel the Google sign-in in your browser, then wait for it to finish.", "MirageSSD"); }
        };
        reinstall = new Button { Text = "Update / reinstall", Left = 34, Top = 358, Width = 200, Height = 32 };
        reinstall.Click += async delegate { await RunInstall(); };
        Controls.Add(reinstall);
        RefreshState();
    }

    private void RefreshState()
    {
        installed = FriendSetup.IsInstalled;
        signedIn = installed && FriendSetup.IsSignedIn;
        reinstall.Visible = installed;
        if (!installed)
        {
            status.Text = "Friend preview: cloud files need internet; cached writes upload in the background. Keep originals until verified.";
            primary.Text = "Install and connect Google Drive";
            primary.Visible = true;
            secondary.Visible = false;
        }
        else if (signedIn)
        {
            status.Text = "MirageSSD is installed and connected. Switch to a different Google account, or sign out to disconnect the drive.";
            primary.Text = "Switch Google account";
            primary.Visible = true;
            secondary.Text = "Sign out";
            secondary.Visible = true;
            LoadAccount();
        }
        else
        {
            status.Text = "MirageSSD is installed but signed out. Sign in to reconnect the drive, or reinstall to set it up again.";
            primary.Text = "Sign in to Google Drive";
            primary.Visible = true;
            secondary.Text = "Reinstall";
            secondary.Visible = true;
        }
    }

    private void LoadAccount()
    {
        Task.Run(delegate
        {
            string account = FriendSetup.SignedInAccount();
            if (!String.IsNullOrEmpty(account) && !IsDisposed && IsHandleCreated)
            {
                BeginInvoke((Action)delegate
                {
                    if (!busy && signedIn) status.Text = "Connected to Google Drive as " + account + ". Switch to a different account, or sign out to disconnect the drive.";
                });
            }
        });
    }

    private void SetWorking(bool working)
    {
        busy = working;
        primary.Enabled = !working;
        secondary.Enabled = !working;
        reinstall.Enabled = !working;
        progress.Visible = working;
    }

    private async void PrimaryClicked(object sender, EventArgs args)
    {
        if (finished) { Close(); return; }
        if (!installed)
        {
            await RunInstall();
        }
        else if (signedIn)
        {
            await RunAccount("SwitchAccount", "Close programs using MirageSSD before continuing.\r\n\r\nYou will sign in with a different Google account in your browser. The drive reconnects when sign-in finishes. Continue?");
        }
        else
        {
            await RunAccount("SignIn", null);
        }
    }

    private async void SecondaryClicked(object sender, EventArgs args)
    {
        if (busy) return;
        if (signedIn)
        {
            await RunAccount("SignOut", "Close programs using MirageSSD before continuing.\r\n\r\nSign out of Google Drive and disconnect the drive? Your cloud files and local cache are kept.");
        }
        else
        {
            await RunInstall();
        }
    }

    private async Task RunInstall()
    {
        SetWorking(true);
        try
        {
            await Task.Run(delegate { FriendSetup.RunPayload(false, UpdateStatus); });
            status.Text = "Ready. MirageSSD is available in This PC and starts when you sign in to Windows. Your drive folder has opened.";
            finished = true;
            primary.Text = "Done";
            secondary.Visible = false;
        }
        catch (Exception error)
        {
            status.Text = "Setup needs attention. Check the message, then retry. If Google blocks access, ask the developer to add your Google email as a test user.";
            MessageBox.Show(this, error.Message, "MirageSSD setup", MessageBoxButtons.OK, MessageBoxIcon.Warning);
            primary.Text = "Retry setup";
        }
        finally { SetWorking(false); }
    }

    private async Task RunAccount(string action, string confirmation)
    {
        if (confirmation != null && MessageBox.Show(this, confirmation, "MirageSSD", MessageBoxButtons.YesNo, MessageBoxIcon.Question) != DialogResult.Yes) return;
        SetWorking(true);
        try
        {
            await Task.Run(delegate { FriendSetup.RunAccountAction(action, UpdateStatus); });
            installed = FriendSetup.IsInstalled;
            signedIn = installed && FriendSetup.IsSignedIn;
            if (signedIn)
            {
                primary.Text = "Switch Google account";
                secondary.Text = "Sign out";
                secondary.Visible = true;
                LoadAccount();
            }
            else
            {
                primary.Text = "Sign in to Google Drive";
                secondary.Text = "Reinstall";
                secondary.Visible = true;
            }
        }
        catch (Exception error)
        {
            status.Text = "That did not finish. Check the message, then try again.";
            MessageBox.Show(this, error.Message, "MirageSSD", MessageBoxButtons.OK, MessageBoxIcon.Warning);
            RefreshState();
        }
        finally { SetWorking(false); }
    }

    private void UpdateStatus(string message)
    {
        if (!IsDisposed && IsHandleCreated) BeginInvoke((Action)delegate { status.Text = message; });
    }
}
