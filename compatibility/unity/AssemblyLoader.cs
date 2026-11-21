using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Security.Cryptography;
using Mono.Cecil;

namespace R2MML.UnityCompatibility
{
    // Called in place of Assembly.LoadFile by the pinned BepInEx Chainloader.
    // PluginInfo.Location still identifies the mod's original external folder.
    public static class AssemblyLoader
    {
        private static readonly Dictionary<string, string> Registered = ReadMap();

        public static Assembly LoadPlugin(string path)
        {
            var source = Path.GetFullPath(path);
            string staged;
            if (!Registered.TryGetValue(source, out staged))
                return Assembly.LoadFile(path);

            if (!FilesMatch(source, staged))
                throw new InvalidOperationException("R2MML Unity registration differs from the active mod: " + source);

            string fullName;
            Guid mvid;
            using (var definition = AssemblyDefinition.ReadAssembly(source))
            {
                fullName = AssemblyName.GetAssemblyName(source).FullName;
                mvid = definition.MainModule.Mvid;
            }
            Assembly match = null;
            foreach (var candidate in AppDomain.CurrentDomain.GetAssemblies())
            {
                if (candidate.IsDynamic || candidate.FullName != fullName)
                    continue;
                if ((candidate.Location != staged && candidate.Location != source) || candidate.ManifestModule.ModuleVersionId != mvid)
                    throw new InvalidOperationException("R2MML found a different assembly loaded as " + fullName);
                if (match != null)
                    throw new InvalidOperationException("R2MML detected multiple Unity registrations for " + fullName);
                match = candidate;
            }
            if (match == null)
                throw new InvalidOperationException("R2MML could not find the assembly registered with Unity: " + fullName);

            Console.Error.WriteLine("R2MML reused Unity assembly {0} ({1})", fullName, match.ManifestModule.ModuleVersionId);
            return match;
        }

        private static Dictionary<string, string> ReadMap()
        {
            var records = new Dictionary<string, string>(StringComparer.Ordinal);
            var mapPath = Environment.GetEnvironmentVariable("R2MML_UNITY_ASSEMBLY_MAP");
            if (String.IsNullOrEmpty(mapPath))
                throw new InvalidOperationException("R2MML Unity registration manifest path is missing.");

            foreach (var line in File.ReadLines(mapPath))
            {
                var paths = line.Split('\t');
                if (paths.Length != 2 || !Path.IsPathRooted(paths[0]) || !Path.IsPathRooted(paths[1]))
                    throw new InvalidDataException("R2MML Unity registration entry is invalid.");
                records.Add(Path.GetFullPath(paths[0]), Path.GetFullPath(paths[1]));
            }
            return records;
        }

        private static bool FilesMatch(string source, string staged)
        {
            using (var sha = SHA256.Create())
            using (var original = File.OpenRead(source))
            using (var copy = File.OpenRead(staged))
            {
                if (original.Length != copy.Length)
                    return false;
                var originalHash = sha.ComputeHash(original);
                var stagedHash = sha.ComputeHash(copy);
                for (var index = 0; index < originalHash.Length; index++)
                    if (originalHash[index] != stagedHash[index])
                        return false;
                return true;
            }
        }
    }
}
