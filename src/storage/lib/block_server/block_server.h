// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_BLOCK_SERVER_BLOCK_SERVER_H_
#define SRC_STORAGE_LIB_BLOCK_SERVER_BLOCK_SERVER_H_

#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <lib/zx/result.h>

#include <memory>

#include "src/storage/lib/block_server/block_server_c.h"

namespace block_server {

using RequestId = internal::RequestId;
using Operation = internal::Operation;
using Request = internal::Request;

struct PartitionInfo {
  uint64_t block_count;
  uint32_t block_size;
  uint8_t type_guid[16];
  uint8_t instance_guid[16];
  const char* name;
};

// Represents a session.  New sessions appear via `OnNewSession`.
class Session {
 public:
  Session(Session&& other) : session_(other.session_) { other.session_ = nullptr; }
  Session& operator=(Session&& other);

  // NOTE: The `BlockServer` destructor will be unblocked before this returns, so take
  // care with any code that runs *after* this returns.
  ~Session();

  // Runs the session (blocking).
  void Run();

  void SendReply(RequestId, zx::result<>);

 private:
  friend class BlockServer;

  explicit Session(const internal::Session* session) : session_(session) {}

  // NOTE: Do not add more members; there are casts in the implementation.
  const internal::Session* session_;
};

// Represents the thread that services all FIDL requests.  This appears via `StartThread`.
class Thread {
 public:
  Thread(Thread&& other) : arg_(other.arg_) { other.arg_ = nullptr; }
  Thread& operator=(Thread&& other) = delete;

  // NOTE: The `BlockServer` destructor will be unblocked before this returns, so take
  // care with any code that runs *after* this returns.
  ~Thread() {
    if (arg_)
      internal::block_server_thread_delete(arg_);
  }

  // Runs the thread (blocking).
  void Run() { internal::block_server_thread(arg_); }

 private:
  friend class BlockServer;

  explicit Thread(const void* arg) : arg_(arg) {}
  const void* arg_;
};

class Interface {
 public:
  // Called to start the thread that processes all FIDL requests.  The implementation must start a
  // thread and then call `Thread::Run`.
  virtual void StartThread(Thread) = 0;

  // Called when a new session is started.  The implementation must start a thread and then call
  // `Session::Run`.  The callback takes ownership of `Session`.
  virtual void OnNewSession(Session) = 0;

  // Called when new requests arrive.  It is OK for this method to block so as to cause push back on
  // the fifo (which is recommended for effective flow control).
  virtual void OnRequests(Session&, cpp20::span<const Request>) = 0;
};

class BlockServer {
 public:
  // Constructs a new server.
  BlockServer(const PartitionInfo&, Interface*);
  BlockServer(const BlockServer&) = delete;
  BlockServer& operator=(const BlockServer&) = delete;

  // Destroys the server.  This will trigger termination and then block until:
  //
  //   1. `Thread::Run()` returns.
  //   2. All `Session` objects have been destroyed i.e. `Session::Run` has returned
  //      *and* `Session` has been destroyed.
  //
  // Once this returns, there will be no subsequent calls via `Interface`.
  ~BlockServer();

  // Serves a new connection.  The FIDL handling is multiplexed onto a single per-server thread.
  void Serve(fidl::ServerEnd<fuchsia_hardware_block_volume::Volume>);

 private:
  Interface* interface_;
  internal::BlockServer* server_;
};

}  // namespace block_server

#endif  // SRC_STORAGE_LIB_BLOCK_SERVER_BLOCK_SERVER_H_