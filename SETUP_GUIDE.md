# Rost Project Setup Guide

## 📁 How to Organize Your Project

After downloading the fixed files, organize them like this:

```
Rost/
├── src/
│   ├── main.rs        ← Kernel entry point
│   ├── memory.rs      ← Memory management
│   ├── cpu.rs         ← CPU setup (GDT/IDT)
│   ├── interrupts.rs  ← Interrupt handlers
│   ├── timer.rs       ← Timer configuration
│   ├── process.rs     ← Process management
│   ├── scheduler.rs   ← Process scheduler
│   ├── ipc.rs         ← Message passing
│   └── console.rs     ← Debug output
├── Cargo.toml         ← Project config (UPDATED)
├── Cargo.lock         ← Dependency lock
├── README.md          ← Main documentation
├── IMPROVEMENTS.md    ← Development roadmap
├── SETUP_SUMMARY.md   ← This guide
└── .gitignore         ← (optional) Git configuration
```

---

## 🚀 Step-by-Step Setup

### Step 1: Create Project Directory

```bash
# Create new directory
mkdir -p ~/projects/Rost
cd ~/projects/Rost

# Create src subdirectory
mkdir -p src
```

### Step 2: Copy Files

Download all files from the outputs folder and organize them:

```bash
# Copy to appropriate locations:
cp Cargo.toml ~/projects/Rost/
cp Cargo.lock ~/projects/Rost/

# Copy documentation
cp README.md ~/projects/Rost/
cp IMPROVEMENTS.md ~/projects/Rost/
cp SETUP_SUMMARY.md ~/projects/Rost/

# Copy Rust source files to src/
cp main.rs ~/projects/Rost/src/
cp memory.rs ~/projects/Rost/src/
cp cpu.rs ~/projects/Rost/src/
cp interrupts.rs ~/projects/Rost/src/
cp timer.rs ~/projects/Rost/src/
cp process.rs ~/projects/Rost/src/
cp scheduler.rs ~/projects/Rost/src/
cp ipc.rs ~/projects/Rost/src/
cp console.rs ~/projects/Rost/src/
```

Or use a script:

```bash
#!/bin/bash
# setup.sh

mkdir -p src
cp ../outputs/Cargo.toml .
cp ../outputs/Cargo.lock .
cp ../outputs/README.md .
cp ../outputs/IMPROVEMENTS.md .
cp ../outputs/SETUP_SUMMARY.md .

for file in main memory cpu interrupts timer process scheduler ipc console; do
  cp ../outputs/${file}.rs src/
done

echo "✓ Project setup complete!"
```

### Step 3: Verify Structure

```bash
# List files to verify
tree -L 2

# Should show:
# .
# ├── Cargo.toml
# ├── Cargo.lock
# ├── README.md
# ├── IMPROVEMENTS.md
# ├── SETUP_SUMMARY.md
# └── src/
#     ├── main.rs
#     ├── memory.rs
#     ├── cpu.rs
#     ├── interrupts.rs
#     ├── timer.rs
#     ├── process.rs
#     ├── scheduler.rs
#     ├── ipc.rs
#     └── console.rs
```

### Step 4: Check Cargo.toml

Verify the `Cargo.toml` file has:

```toml
[package]
name = "Rost"                    # ✓ Should be "Rost"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "Rost"                    # ✓ Should be "Rost"
path = "src/main.rs"             # ✓ Should be "src/main.rs"
```

---

## 🔧 Building the Kernel

### Prerequisites

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add UEFI target
rustup target add x86_64-unknown-uefi

# Verify installation
cargo --version
rustc --version
```

### Build Commands

```bash
# Navigate to project directory
cd ~/projects/Rost

# Development build (faster, not optimized)
cargo build --target x86_64-unknown-uefi

# Release build (slower build, faster runtime, smaller size)
cargo build --release --target x86_64-unknown-uefi

# Clean previous builds
cargo clean
```

### Output Location

After building, the kernel image will be at:

**Development**: `target/x86_64-unknown-uefi/debug/Rost.efi`

**Release**: `target/x86_64-unknown-uefi/release/Rost.efi`

---

## 🖥️ Running on QEMU

### Install QEMU and OVMF

**macOS:**
```bash
brew install qemu
# OVMF usually comes with QEMU
```

**Ubuntu/Debian:**
```bash
sudo apt-get update
sudo apt-get install qemu-system-x86 ovmf
```

**Fedora/RHEL:**
```bash
sudo dnf install qemu-system-x86 edk2-ovmf
```

**Arch Linux:**
```bash
sudo pacman -S qemu edk2-ovmf
```

### Find OVMF Path

```bash
# macOS
ls /usr/local/Cellar/qemu/*/share/qemu/

# Linux
ls /usr/share/OVMF/
# or
ls /usr/share/edk2/x64/
# or
find /usr -name "OVMF*.fd" 2>/dev/null
```

### Create Boot Image

```bash
# Create 1GB disk image
dd if=/dev/zero of=boot.img bs=1M count=1024

# Format as FAT32
mkdosfs boot.img

# Mount the image
mkdir -p mnt_boot
sudo mount -o loop boot.img mnt_boot

# Create EFI directory structure
sudo mkdir -p mnt_boot/EFI/BOOT

# Copy kernel
sudo cp target/x86_64-unknown-uefi/release/Rost.efi mnt_boot/EFI/BOOT/BOOTX64.EFI

# Unmount
sudo umount mnt_boot
rm -rf mnt_boot
```

### Launch QEMU

```bash
qemu-system-x86_64 \
  -bios /path/to/OVMF_CODE.fd \
  -drive file=boot.img,format=raw \
  -m 512M \
  -serial stdio
```

**Example for Ubuntu:**
```bash
qemu-system-x86_64 \
  -bios /usr/share/OVMF/OVMF_CODE.fd \
  -drive file=boot.img,format=raw \
  -m 512M \
  -serial stdio
```

### Expected Output

```
╔════════════════════════════════════╗
║   Rost Microkernel v0.1.0         ║
║   UEFI-based x86_64 Kernel        ║
╚════════════════════════════════════╝

=== INITIALIZATION SEQUENCE ===

[1/7] Memory Management
      └─ Kernel heap:     0x100000 (1 MB)
      └─ Page tables:     Ready
      └─ Status:          ✓ OK

[2/7] CPU Setup (GDT/IDT)
      └─ GDT loaded:      3 selectors (null, code, data)
      └─ IDT loaded:      256 gates registered
      └─ Status:          ✓ OK

[3/7] Interrupt Handlers
      └─ Exception 0:     Division by zero
      └─ Exception 13:    General protection fault
      └─ Exception 14:    Page fault
      └─ Interrupt 32:    Timer (PIT)
      └─ Status:          ✓ OK

[4/7] System Timer
      └─ PIT frequency:   100 Hz (10 ms ticks)
      └─ PIC configured:  Master & Slave
      └─ Status:          ✓ OK

[5/7] Process Management
      └─ Process stack:   0x2000 (8 KB)
      └─ Max processes:   32
      └─ Status:          ✓ OK

[6/7] Scheduler
      └─ Algorithm:       Round-robin
      └─ Time quantum:    10 ms
      └─ First process:   PID 0x1
      └─ Status:          ✓ OK

[7/7] Inter-Process Communication
      └─ Queue size:      16 messages
      └─ Msg per queue:   8 u64 fields
      └─ Status:          ✓ OK

╔════════════════════════════════════╗
║        KERNEL INITIALIZATION      ║
║             COMPLETE              ║
╚════════════════════════════════════╝

✓ Interrupts enabled
✓ Entering kernel idle loop

Rost is running...
```

---

## 🔍 Troubleshooting

### Problem: Build Fails with "target not installed"

**Solution**: Install UEFI target
```bash
rustup target add x86_64-unknown-uefi
```

### Problem: Can't Find OVMF

**Solution**: Locate it
```bash
# Find OVMF
find /usr -name "OVMF*.fd" 2>/dev/null
find /opt -name "OVMF*.fd" 2>/dev/null
# On macOS
ls /usr/local/Cellar/qemu/*/share/qemu/
```

### Problem: Can't Mount boot.img

**Solution**: Use different method
```bash
# Try without loop device
mkdir -p mnt_boot
sudo mount boot.img mnt_boot -o offset=0

# Or use udisksctl
udisksctl loop-setup -f boot.img
udisksctl mount -b /dev/loop0
```

### Problem: QEMU Won't Start

**Solution**: Check path and syntax
```bash
# Verify paths exist
ls -la /path/to/OVMF_CODE.fd
ls -la boot.img

# Test QEMU
qemu-system-x86_64 --version

# Try with explicit machine type
qemu-system-x86_64 -M pc \
  -bios /path/to/OVMF_CODE.fd \
  -drive file=boot.img,format=raw \
  -m 512M
```

---

## 📝 Using Git (Optional)

### Initialize Repository

```bash
cd ~/projects/Rost
git init
git add .
git commit -m "Initial Rost microkernel setup"
```

### Create .gitignore

```bash
cat > .gitignore << 'EOF'
# Rust build artifacts
target/
Cargo.lock
*.rlib
*.rmeta

# IDE
.idea/
.vscode/
*.swp
*.swo
*~

# Boot images
boot.img
mnt_boot/

# OS-specific
.DS_Store
Thumbs.db
EOF

git add .gitignore
git commit -m "Add .gitignore"
```

---

## 🎯 Next Development Steps

After successful setup:

1. **Read the documentation**
   ```bash
   less README.md           # Understand the kernel
   less IMPROVEMENTS.md     # See what to build next
   ```

2. **Explore the code**
   ```bash
   # Start with the entry point
   less src/main.rs
   
   # Then explore modules
   less src/scheduler.rs    # How scheduling works
   less src/memory.rs       # How memory is managed
   less src/interrupts.rs   # How interrupts are handled
   ```

3. **Choose a feature to implement**
   See IMPROVEMENTS.md for prioritized list

4. **Start coding**
   - Create a feature branch
   - Implement incrementally
   - Test frequently on QEMU

---

## 📊 Project Status After Setup

After completing this guide, you'll have:

| Item | Status |
|------|--------|
| Project structure | ✓ Organized |
| Build files | ✓ Configured |
| Documentation | ✓ Complete |
| Kernel code | ✓ Ready |
| Build system | ✓ Working |
| QEMU setup | ✓ Ready |
| Boot image | ✓ Creatable |
| Development environment | ✓ Complete |

---

## 💡 Development Workflow

Typical daily workflow:

```bash
# 1. Make changes to source
vim src/scheduler.rs

# 2. Build kernel
cargo build --release --target x86_64-unknown-uefi

# 3. Create/update boot image
dd if=/dev/zero of=boot.img bs=1M count=1024
mkdosfs boot.img
mkdir mnt_boot
sudo mount -o loop boot.img mnt_boot
sudo mkdir -p mnt_boot/EFI/BOOT
sudo cp target/x86_64-unknown-uefi/release/Rost.efi mnt_boot/EFI/BOOT/BOOTX64.EFI
sudo umount mnt_boot

# 4. Test in QEMU
qemu-system-x86_64 \
  -bios /usr/share/OVMF/OVMF_CODE.fd \
  -drive file=boot.img,format=raw \
  -m 512M \
  -serial stdio

# 5. Commit changes
git add .
git commit -m "Feature: describe what you changed"
```

---

## 🎓 Learning Path

After setup, follow this order:

1. **Week 1**: Understand kernel structure
   - Read all `.md` files
   - Study `main.rs` initialization
   - Review module architecture

2. **Week 2**: Implement context switching
   - Study CPU context saving
   - Write assembly code
   - Test process switching

3. **Week 3**: Add system calls
   - Define syscall interface
   - Implement dispatcher
   - Add 5-10 basic syscalls

4. **Week 4**: Create filesystem service
   - Implement FAT32 reader
   - Run as userspace process
   - Test file operations

---

## ✅ Verification Checklist

After setup, verify everything:

- [ ] Project directory exists: `~/projects/Rost/`
- [ ] All files in correct locations
- [ ] `Cargo.toml` has correct package name
- [ ] `src/main.rs` exists and compiles
- [ ] `cargo build` completes successfully
- [ ] OVMF firmware is installed
- [ ] QEMU is installed and works
- [ ] Boot image can be created
- [ ] QEMU boots the kernel
- [ ] Initialization output appears
- [ ] "Rost is running..." message shows

---

## 📞 Need Help?

- **Build issues**: Check Rust installation: `rustc --version`
- **QEMU issues**: Verify OVMF path and QEMU installation
- **Kernel issues**: Check console output, read IMPROVEMENTS.md
- **Code questions**: Review relevant module documentation

---

## 🎉 Success!

Once you've completed this setup and successfully booted Rost on QEMU, you have:

✓ A working microkernel project
✓ Complete development environment  
✓ Clear roadmap for improvements
✓ Foundation for kernel research or education

**Next**: Read IMPROVEMENTS.md and start implementing the next feature!

---

**Setup Guide Version**: 1.0  
**Rost Version**: 0.1.0  
**Last Updated**: March 7, 2026
