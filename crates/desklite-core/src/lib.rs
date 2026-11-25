pub mod clamp;
pub mod layout;
pub mod store;
pub mod topology;

pub use clamp::{
    MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, clamp_point_to_monitors, clamp_rect_to_monitors,
    clamp_rect_to_work_area, nearest_monitor,
};
pub use layout::{
    CURRENT_SCHEMA_VERSION, DEFAULT_BACKDROP_ALPHA, DesktopIconPos, FenceItem, FenceKind,
    FenceSpec, ItemKind, LayoutPreferences, MigrationReport, NativeLayoutConfig, TopologySnapshot,
};
pub use store::{StoreError, atomic_write_bytes, load_native_layout, save_native_layout};
pub use topology::{DisplayTopology, MonitorDescriptor, Point, Rect, TopologyError, TopologyKey};
