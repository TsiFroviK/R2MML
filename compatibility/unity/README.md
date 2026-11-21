# Unity script registration

R2MML reads the `MonoScript` entries in enabled mods' UnityFS bundles and registers the referenced assemblies in a temporary copy of the Risk2Mac player. The managed helper reuses the assembly Unity has already loaded so BepInEx and Unity use the same types and static state. BepInEx keeps the mod's external location for loading its assets.

## Rebuild the managed adapter

Use the official BepInEx 5.4.23.6 macOS universal distribution, a .NET 8 SDK and a Mono-compatible C# compiler. Run from this directory. [BepInEx 5.4.23.6](https://github.com/BepInEx/BepInEx/releases/tag/v5.4.23.6)

```sh
core=/path/to/extracted/BepInEx/core
mcs -optimize -target:library -r:"$core/Mono.Cecil.dll" \
  -out:R2MML.UnityCompatibility.dll AssemblyLoader.cs
dotnet run --project BuildAdapter.csproj -p:CecilPath="$core/Mono.Cecil.dll" -- \
  "$core/BepInEx.dll" R2MML.UnityCompatibility.dll BepInEx.UnityCompatibility.dll
```

The generator verifies the official input hash and changes one Chainloader call. The distributed `BepInEx.UnityCompatibility.dll` uses the upstream MIT license. [BepInEx source](https://github.com/BepInEx/BepInEx/tree/v5.4.23.6) · [License](BepInEx-LICENSE.txt)

| Checked-in assembly | SHA-256 |
| --- | --- |
| `R2MML.UnityCompatibility.dll` | `d7f2edd1097d4e7d916d429bfa7332eac316eea82d1c23ea5457f66df97f628d` |
| `BepInEx.UnityCompatibility.dll` | `3d75e2d020ae99c7817023e527fed945bcdcb9ceb18ef20fbade3216edf33e1f` |
