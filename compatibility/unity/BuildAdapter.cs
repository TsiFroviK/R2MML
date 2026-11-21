using System.Security.Cryptography;
using Mono.Cecil;
using Mono.Cecil.Cil;

if (args.Length != 3)
    throw new ArgumentException("Supply the official BepInEx.dll, compiled compatibility helper, and output filename.");

var originalPath = Path.GetFullPath(args[0]);
var helperPath = Path.GetFullPath(args[1]);
var outputPath = Path.GetFullPath(args[2]);
const string pinnedSha = "780F175821781C14FDC349BF214603B7BAB594AC556AECB1F6679B80248133F0";
if (Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(originalPath))) != pinnedSha)
    throw new InvalidOperationException("Expected the verified BepInEx 5.4.23.6 macOS runtime.");

using var original = AssemblyDefinition.ReadAssembly(originalPath);
using var helper = AssemblyDefinition.ReadAssembly(helperPath);
var module = original.MainModule;
var start = module.GetType("BepInEx.Bootstrap.Chainloader").Methods.Single(method => method.Name == "Start");
var loadFile = start.Body.Instructions.Single(instruction => instruction.OpCode == OpCodes.Call &&
    instruction.Operand is MethodReference method &&
    method.FullName == "System.Reflection.Assembly System.Reflection.Assembly::LoadFile(System.String)");
var loadPlugin = helper.MainModule.GetType("R2MML.UnityCompatibility.AssemblyLoader").Methods.Single(method =>
    method.IsPublic && method.IsStatic && method.FullName ==
    "System.Reflection.Assembly R2MML.UnityCompatibility.AssemblyLoader::LoadPlugin(System.String)");
var originalScope = ((MethodReference)loadFile.Operand).ReturnType.Scope.Name;
if (loadPlugin.ReturnType.Scope.Name != originalScope || loadPlugin.Parameters[0].ParameterType.Scope.Name != originalScope)
    throw new InvalidOperationException("Compile the compatibility helper against the Mono framework.");

loadFile.Operand = module.ImportReference(loadPlugin);
using (var output = new FileStream(outputPath, FileMode.CreateNew))
    original.Write(output);

Console.WriteLine(Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(outputPath))).ToLowerInvariant());
