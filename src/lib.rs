use pyo3::prelude::*;
use pyo3::exceptions::PyIOError;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use memmap2::Mmap;
use bytemuck::{Pod, Zeroable, cast_slice};

// --- DATA LAYOUT ---
// Exactly 24 bytes per record. No padding, no varints, no SQL overhead.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct HistoryRecord {
    timestamp: u64,   // 8 bytes (Unix epoch)
    preset_id: u32,   // 4 bytes
    return_pct: f32,  // 4 bytes
    _padding: u64,    // 8 bytes (Alignment padding to make it 24 bytes)
}

#[pyclass]
struct NanoDB {
    file: File,
    mmap: Option<Mmap>,
    record_count: usize,
}

#[pymethods]
impl NanoDB {
    #[new]
    fn new(path: &str) -> PyResult<Self> {
        // Open file for reading and appending
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| PyIOError::new_err(e.to_string()))?;

        let metadata = file.metadata().map_err(|e| PyIOError::new_err(e.to_string()))?;
        let file_size = metadata.len() as usize;
        
        // Calculate how many 24-byte records exist
        let record_count = file_size / std::mem::size_of::<HistoryRecord>();

        // Memory map the file for Zero-Copy reads
        let mmap = if file_size > 0 {
            Some(unsafe { Mmap::map(&file).map_err(|e| PyIOError::new_err(e.to_string()))? })
        } else {
            None
        };

        Ok(NanoDB { file, mmap, record_count })
    }

    /// O(1) Append. No B-Trees. Just raw bytes to disk.
    fn log_run(&mut self, timestamp: u64, preset_id: u32, return_pct: f32) -> PyResult<()> {
        let record = HistoryRecord {
            timestamp,
            preset_id,
            return_pct,
            _padding: 0,
        };
        
        // Write raw bytes directly to the file descriptor
        let bytes = bytemuck::bytes_of(&record);
        self.file.write_all(bytes).map_err(|e| PyIOError::new_err(e.to_string()))?;
        self.file.flush().map_err(|e| PyIOError::new_err(e.to_string()))?;
        
        // Remap memory to include the new record
        self.record_count += 1;
        self.mmap = Some(unsafe { Mmap::map(&self.file).map_err(|e| PyIOError::new_err(e.to_string()))? });
        
        Ok(())
    }

    /// Zero-Copy Read. Returns the last N runs for a specific preset.
    /// SQL would parse a query, scan a B-Tree, and allocate memory.
    /// We just do pointer arithmetic.
    fn get_recent_runs(&self, preset_id: u32, limit: usize) -> PyResult<Vec<(u64, f32)>> {
        let mut results = Vec::new();
        
        if let Some(mmap) = &self.mmap {
            // Cast the raw memory map directly into an array of structs
            let records: &[HistoryRecord] = cast_slice(mmap);
            
            // Iterate backwards (most recent first)
            for i in (0..self.record_count).rev() {
                let record = records[i];
                if record.preset_id == preset_id {
                    results.push((record.timestamp, record.return_pct));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        
        Ok(results)
    }
}

#[pymodule]
fn nanodb(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<NanoDB>()?;
    Ok(())
}