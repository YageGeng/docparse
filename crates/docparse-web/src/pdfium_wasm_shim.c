typedef unsigned char uint8_t;
typedef unsigned short uint16_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;
typedef int int32_t;
typedef long long int64_t;
typedef unsigned long size_t;
typedef unsigned long uintptr_t;

#define WASI_ESUCCESS 0
#define WASI_EBADF 8
#define WASI_ENOSYS 52

static void zero_bytes(void *ptr, size_t len) {
  uint8_t *bytes = (uint8_t *)ptr;
  for (size_t index = 0; index < len; index += 1) {
    bytes[index] = 0;
  }
}

int getpid(void) {
  return 1;
}

int pthread_mutex_init(void *mutex, const void *attr) {
  (void)mutex;
  (void)attr;
  return 0;
}

int pthread_mutex_destroy(void *mutex) {
  (void)mutex;
  return 0;
}

int pthread_mutex_lock(void *mutex) {
  (void)mutex;
  return 0;
}

int pthread_mutex_unlock(void *mutex) {
  (void)mutex;
  return 0;
}

int32_t __imported_wasi_snapshot_preview1_environ_get(uint32_t environ,
                                                      uint32_t environ_buf) {
  (void)environ;
  (void)environ_buf;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_environ_sizes_get(uint32_t *count,
                                                            uint32_t *buf_size) {
  *count = 0;
  *buf_size = 0;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_clock_time_get(uint32_t clock_id,
                                                         uint64_t precision,
                                                         uint64_t *time) {
  (void)clock_id;
  (void)precision;
  *time = 0;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_fd_close(uint32_t fd) {
  (void)fd;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_fd_fdstat_get(uint32_t fd,
                                                        void *stat) {
  (void)fd;
  zero_bytes(stat, 24);
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_fd_fdstat_set_flags(uint32_t fd,
                                                              uint16_t flags) {
  (void)fd;
  (void)flags;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_fd_prestat_get(uint32_t fd,
                                                         void *prestat) {
  (void)fd;
  (void)prestat;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_fd_prestat_dir_name(uint32_t fd,
                                                              uint32_t path,
                                                              uint32_t path_len) {
  (void)fd;
  (void)path;
  (void)path_len;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_fd_read(uint32_t fd, uint32_t iovs,
                                                  uint32_t iovs_len,
                                                  uint32_t *nread) {
  (void)fd;
  (void)iovs;
  (void)iovs_len;
  *nread = 0;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_fd_readdir(uint32_t fd, uint32_t buf,
                                                     uint32_t buf_len,
                                                     uint64_t cookie,
                                                     uint32_t *bufused) {
  (void)fd;
  (void)buf;
  (void)buf_len;
  (void)cookie;
  *bufused = 0;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_fd_seek(uint32_t fd, int64_t offset,
                                                  uint32_t whence,
                                                  uint64_t *newoffset) {
  (void)fd;
  (void)offset;
  (void)whence;
  *newoffset = 0;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_fd_write(uint32_t fd, uint32_t iovs,
                                                   uint32_t iovs_len,
                                                   uint32_t *nwritten) {
  (void)fd;
  uint32_t total = 0;
  uint32_t *iov = (uint32_t *)(uintptr_t)iovs;
  for (uint32_t index = 0; index < iovs_len; index += 1) {
    total += iov[(index * 2) + 1];
  }
  *nwritten = total;
  return WASI_ESUCCESS;
}

int32_t __imported_wasi_snapshot_preview1_path_filestat_get(
    uint32_t fd, uint32_t flags, uint32_t path, uint32_t path_len, void *stat) {
  (void)fd;
  (void)flags;
  (void)path;
  (void)path_len;
  zero_bytes(stat, 64);
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_path_open(
    uint32_t fd, uint32_t dirflags, uint32_t path, uint32_t path_len,
    uint32_t oflags, uint64_t fs_rights_base, uint64_t fs_rights_inheriting,
    uint16_t fdflags, uint32_t *opened_fd) {
  (void)fd;
  (void)dirflags;
  (void)path;
  (void)path_len;
  (void)oflags;
  (void)fs_rights_base;
  (void)fs_rights_inheriting;
  (void)fdflags;
  *opened_fd = 0;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_path_remove_directory(uint32_t fd,
                                                                uint32_t path,
                                                                uint32_t path_len) {
  (void)fd;
  (void)path;
  (void)path_len;
  return WASI_EBADF;
}

int32_t __imported_wasi_snapshot_preview1_path_unlink_file(uint32_t fd,
                                                           uint32_t path,
                                                           uint32_t path_len) {
  (void)fd;
  (void)path;
  (void)path_len;
  return WASI_EBADF;
}

void __imported_wasi_snapshot_preview1_proc_exit(uint32_t exit_code) {
  (void)exit_code;
  for (;;) {
  }
}
