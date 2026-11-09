# Third-party components

R2MML's application and packaging tool are written in Rust. The repository
contains no Risk of Rain 2 assets, assemblies, Unity player, Steam library,
mod archive, or BepInEx binary. The icon is original project artwork.

Rust dependencies retain their respective licenses. `Cargo.lock` pins the
dependency graph. Release bundles include `Contents/Resources/DEPENDENCY-LICENSES.txt`
with package versions, declared licenses, upstream links, and license files
collected from their Cargo source distributions. This includes egui/eframe's
bundled font licenses. The application's MIT license does not replace these.

At the first modded launch, R2MML downloads the official
[BepInEx 5.4.23.6 macOS universal release](https://github.com/BepInEx/BepInEx/releases/tag/v5.4.23.6),
verifies its pinned SHA-256, and stores it outside the game. BepInEx is
[MIT licensed](https://github.com/BepInEx/BepInEx/blob/v5.4.23.6/LICENSE);
[Unity Doorstop](https://github.com/NeighTools/UnityDoorstop) is LGPL-2.1 licensed.
Their files are downloaded unmodified, with included license notices retained.
Neither is bundled into R2MML releases. Their corresponding source is available
from the linked upstream projects and release tags.

Thunderstore packages are downloaded from Thunderstore only when selected
by the user or required as dependencies. Each mod keeps its own license,
manifest, and documentation in the local package store. R2MML does not
relicense or republish these packages. Package icons and descriptions are
fetched from Thunderstore for display and are not included in this source tree.

R2MML is an independent community project, not affiliated with or endorsed by
Gearbox, Valve, Thunderstore, or the BepInEx project. Risk of Rain 2 and related
names belong to their respective owners.
