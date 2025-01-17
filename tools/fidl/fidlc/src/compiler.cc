// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/compiler.h"

#include <zircon/assert.h>

#include <algorithm>

#define BORINGSSL_NO_CXX
#include <openssl/sha.h>

#include "tools/fidl/fidlc/src/attribute_schema.h"
#include "tools/fidl/fidlc/src/availability_step.h"
#include "tools/fidl/fidlc/src/compile_step.h"
#include "tools/fidl/fidlc/src/consume_step.h"
#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/names.h"
#include "tools/fidl/fidlc/src/replacement_step.h"
#include "tools/fidl/fidlc/src/resolve_step.h"
#include "tools/fidl/fidlc/src/type_shape_step.h"
#include "tools/fidl/fidlc/src/verify_steps.h"

namespace fidlc {

uint64_t Sha256MethodHasher(std::string_view selector) {
  uint8_t digest[SHA256_DIGEST_LENGTH];
  SHA256(reinterpret_cast<const uint8_t*>(selector.data()), selector.size(), digest);
  // The following dance ensures that we treat the bytes as a little-endian
  // int64 regardless of host byte order.
  // clang-format off
  uint64_t ordinal =
      static_cast<uint64_t>(digest[0]) |
      static_cast<uint64_t>(digest[1]) << 8 |
      static_cast<uint64_t>(digest[2]) << 16 |
      static_cast<uint64_t>(digest[3]) << 24 |
      static_cast<uint64_t>(digest[4]) << 32 |
      static_cast<uint64_t>(digest[5]) << 40 |
      static_cast<uint64_t>(digest[6]) << 48 |
      static_cast<uint64_t>(digest[7]) << 56;
  // clang-format on
  return ordinal & 0x7fffffffffffffff;
}

Compiler::Compiler(Libraries* all_libraries, const VersionSelection* version_selection,
                   MethodHasher method_hasher, ExperimentalFlagSet experimental_flags)
    : reporter_(all_libraries->reporter()),
      library_(std::make_unique<Library>()),
      all_libraries_(all_libraries),
      version_selection(version_selection),
      method_hasher_(std::move(method_hasher)),
      experimental_flags_(experimental_flags),
      typespace_start_index_(all_libraries->typespace()->types().size()) {}

bool Compiler::ConsumeFile(std::unique_ptr<File> file) {
  return ConsumeStep(this, std::move(file)).Run();
}

bool Compiler::Compile() {
  auto checkpoint = reporter_->Checkpoint();

  if (!AvailabilityStep(this).Run())
    return false;
  if (!ResolveStep(this).Run())
    return false;
  if (!CompileStep(this).Run())
    return false;
  if (!TypeShapeStep(this).Run())
    return false;
  if (!ReplacementStep(this).Run())
    return false;
  if (!VerifyResourcenessStep(this).Run())
    return false;
  if (!VerifyHandleTransportStep(this).Run())
    return false;
  if (!VerifyAttributesStep(this).Run())
    return false;
  if (!VerifyDependenciesStep(this).Run())
    return false;

  if (!all_libraries_->Insert(std::move(library_)))
    return false;

  ZX_ASSERT_MSG(checkpoint.NoNewErrors(), "errors should have caused an early return");
  return true;
}

bool Compiler::Step::Run() {
  auto checkpoint = reporter()->Checkpoint();
  RunImpl();
  return checkpoint.NoNewErrors();
}

Typespace* Compiler::Step::typespace() { return compiler_->all_libraries_->typespace(); }

cpp20::span<const std::unique_ptr<Type>> Compiler::Step::created_types() {
  auto& types = typespace()->types();
  auto start = static_cast<ssize_t>(compiler_->typespace_start_index_);
  return cpp20::span(types.begin() + start, types.end());
}

VirtualSourceFile* Compiler::Step::generated_source_file() {
  return compiler_->all_libraries_->generated_source_file();
}

bool Libraries::Insert(std::unique_ptr<Library> library) {
  auto [_, inserted] = libraries_by_name_.try_emplace(library->name, library.get());
  if (!inserted) {
    return reporter_->Fail(ErrMultipleLibrariesWithSameName, library->name_spans[0], library->name);
  }
  libraries_.push_back(std::move(library));
  return true;
}

Library* Libraries::Lookup(std::string_view library_name) const {
  auto iter = libraries_by_name_.find(library_name);
  return iter == libraries_by_name_.end() ? nullptr : iter->second;
}

void Libraries::Remove(const Library* library) {
  auto num_removed = libraries_by_name_.erase(library->name);
  ZX_ASSERT_MSG(num_removed == 1, "library not in libraries_by_name_");
  auto iter = std::find_if(libraries_.begin(), libraries_.end(),
                           [&](auto& lib) { return lib.get() == library; });
  ZX_ASSERT_MSG(iter != libraries_.end(), "library not in libraries_");
  libraries_.erase(iter);
}

AttributeSchema& Libraries::AddAttributeSchema(std::string name) {
  auto [it, inserted] = attribute_schemas_.try_emplace(std::move(name));
  ZX_ASSERT_MSG(inserted, "do not add schemas twice");
  return it->second;
}

std::set<const Library*, LibraryComparator> Libraries::Unused() const {
  std::set<const Library*, LibraryComparator> unused;
  auto target = libraries_.back().get();
  ZX_ASSERT_MSG(target, "must have inserted at least one library");
  for (auto& library : libraries_) {
    if (library.get() != target) {
      unused.insert(library.get());
    }
  }
  std::set<const Library*> worklist = {target};
  while (!worklist.empty()) {
    auto it = worklist.begin();
    auto next = *it;
    worklist.erase(it);
    for (const auto dependency : next->dependencies.all()) {
      unused.erase(dependency);
      worklist.insert(dependency);
    }
  }
  return unused;
}

static size_t EditDistance(std::string_view sequence1, std::string_view sequence2) {
  size_t s1_length = sequence1.length();
  size_t s2_length = sequence2.length();
  std::vector<size_t> row1(s1_length + 1);
  std::vector<size_t> row2(s1_length + 1);
  size_t* last_row = row1.data();
  size_t* this_row = row2.data();
  for (size_t j = 0; j < s2_length; j++) {
    this_row[0] = j + 1;
    auto s2c = sequence2[j];
    for (size_t i = 1; i <= s1_length; i++) {
      auto s1c = sequence1[i - 1];
      this_row[i] = std::min(std::min(last_row[i] + 1, this_row[i - 1] + 1),
                             last_row[i - 1] + (s1c == s2c ? 0 : 1));
    }
    std::swap(last_row, this_row);
  }
  return last_row[s1_length];
}

const AttributeSchema& Libraries::RetrieveAttributeSchema(const Attribute* attribute) const {
  auto attribute_name = attribute->name.data();
  auto iter = attribute_schemas_.find(attribute_name);
  if (iter != attribute_schemas_.end()) {
    return iter->second;
  }
  return AttributeSchema::kUserDefined;
}

void Libraries::WarnOnAttributeTypo(const Attribute* attribute) const {
  auto attribute_name = attribute->name.data();
  auto iter = attribute_schemas_.find(attribute_name);
  if (iter != attribute_schemas_.end()) {
    return;
  }
  for (const auto& [suspected_name, schema] : attribute_schemas_) {
    auto supplied_name = attribute_name;
    auto edit_distance = EditDistance(supplied_name, suspected_name);
    if (0 < edit_distance && edit_distance < 2) {
      reporter_->Warn(WarnAttributeTypo, attribute->span, supplied_name, suspected_name);
    }
  }
}

static std::vector<const Struct*> ExternalStructs(const Library* target_library,
                                                  const std::vector<const Protocol*>& protocols) {
  // Ensure deterministic ordering.
  auto ordering = [](const Struct* a, const Struct* b) {
    return FullyQualifiedName(a->name) < FullyQualifiedName(b->name);
  };
  std::set<const Struct*, decltype(ordering)> external_structs(ordering);

  auto visit = [&](const Type* type) {
    if (type->kind == Type::Kind::kIdentifier) {
      auto decl = static_cast<const IdentifierType*>(type)->type_decl;
      if (decl->kind == Decl::Kind::kStruct && type->name.library() != target_library) {
        external_structs.insert(static_cast<const Struct*>(decl));
      }
    }
  };

  for (auto& protocol : protocols) {
    for (auto& method_with_info : protocol->all_methods) {
      if (auto& request = method_with_info.method->maybe_request) {
        visit(request->type);
      }
      if (auto& response = method_with_info.method->maybe_response) {
        visit(response->type);
      }
      if (auto union_decl = method_with_info.method->maybe_result_union) {
        for (auto& member : union_decl->members) {
          visit(member.type_ctor->type);
        }
      }
    }
  }

  return std::vector(external_structs.begin(), external_structs.end());
}

namespace {

// Helper class to calculate Compilation::direct_and_composed_dependencies.
class CalcDependencies {
 public:
  std::set<const Library*, LibraryComparator> From(std::vector<const Decl*>& roots) && {
    for (const Decl* decl : roots) {
      VisitDecl(decl);
    }
    return std::move(deps_);
  }

 private:
  std::set<const Library*, LibraryComparator> deps_;

  void VisitDecl(const Decl* decl) {
    switch (decl->kind) {
      case Decl::Kind::kBuiltin: {
        ZX_PANIC("unexpected builtin");
      }
      case Decl::Kind::kBits: {
        auto bits_decl = static_cast<const Bits*>(decl);
        VisitTypeConstructor(bits_decl->subtype_ctor.get());
        for (auto& member : bits_decl->members) {
          VisitConstant(member.value.get());
        }
        break;
      }
      case Decl::Kind::kConst: {
        auto const_decl = static_cast<const Const*>(decl);
        VisitTypeConstructor(const_decl->type_ctor.get());
        VisitConstant(const_decl->value.get());
        break;
      }
      case Decl::Kind::kEnum: {
        auto enum_decl = static_cast<const Enum*>(decl);
        VisitTypeConstructor(enum_decl->subtype_ctor.get());
        for (auto& member : enum_decl->members) {
          VisitConstant(member.value.get());
        }
        break;
      }
      case Decl::Kind::kProtocol: {
        auto protocol_decl = static_cast<const Protocol*>(decl);
        // Make sure we insert libraries for composed protocols, even if those
        // protocols are empty (so we don't get the dependency from a method).
        for (auto& composed_protocol : protocol_decl->composed_protocols) {
          VisitReference(composed_protocol.reference);
        }
        for (auto& method_with_info : protocol_decl->all_methods) {
          auto method = method_with_info.method;
          // Make sure we insert libraries for all transitive composed
          // protocols, even if they have no methods with payloads.
          deps_.insert(method_with_info.owning_protocol->name.library());
          if (auto request = method->maybe_request.get()) {
            VisitTypeConstructorAndStructFields(request);
          }
          if (auto union_decl = method->maybe_result_union) {
            for (const auto& member : union_decl->members) {
              VisitTypeConstructorAndStructFields(member.type_ctor.get());
            }
          } else if (auto response = method->maybe_response.get()) {
            VisitTypeConstructorAndStructFields(response);
          }
        }
        break;
      }
      case Decl::Kind::kResource: {
        auto resource_decl = static_cast<const Resource*>(decl);
        VisitTypeConstructor(resource_decl->subtype_ctor.get());
        for (auto& property : resource_decl->properties) {
          VisitTypeConstructor(property.type_ctor.get());
        }
        break;
      }
      case Decl::Kind::kService: {
        auto service_decl = static_cast<const Service*>(decl);
        for (auto& member : service_decl->members) {
          VisitTypeConstructor(member.type_ctor.get());
        }
        break;
      }
      case Decl::Kind::kStruct: {
        auto struct_decl = static_cast<const Struct*>(decl);
        for (auto& member : struct_decl->members) {
          VisitTypeConstructor(member.type_ctor.get());
          if (auto& value = member.maybe_default_value) {
            VisitConstant(value.get());
          }
        }
        break;
      }
      case Decl::Kind::kTable: {
        auto table_decl = static_cast<const Table*>(decl);
        for (auto& member : table_decl->members) {
          VisitTypeConstructor(member.type_ctor.get());
        }
        break;
      }
      case Decl::Kind::kAlias: {
        auto alias_decl = static_cast<const Alias*>(decl);
        VisitTypeConstructor(alias_decl->partial_type_ctor.get());
        break;
      }
      case Decl::Kind::kNewType: {
        auto new_type_decl = static_cast<const NewType*>(decl);
        VisitTypeConstructor(new_type_decl->type_ctor.get());
        break;
      }
      case Decl::Kind::kUnion: {
        auto union_decl = static_cast<const Union*>(decl);
        for (auto& member : union_decl->members) {
          VisitTypeConstructor(member.type_ctor.get());
        }
        break;
      }
      case Decl::Kind::kOverlay: {
        auto overlay_decl = static_cast<const Overlay*>(decl);
        for (auto& member : overlay_decl->members) {
          VisitTypeConstructor(member.type_ctor.get());
        }
      }
    }
  }

  // Like `VisitTypeConstructor`, but also visits the struct fields if it is a
  // struct. We use this for method requests and responses because some bindings
  // flatten struct requests/responses into lists of parameters.
  void VisitTypeConstructorAndStructFields(const TypeConstructor* type_ctor) {
    VisitTypeConstructor(type_ctor);
    if (type_ctor->type->kind == Type::Kind::kIdentifier) {
      auto type_decl = static_cast<const IdentifierType*>(type_ctor->type)->type_decl;
      if (type_decl->kind == Decl::Kind::kStruct) {
        VisitDecl(static_cast<const Struct*>(type_decl));
      }
    }
  }

  void VisitTypeConstructor(const TypeConstructor* type_ctor) {
    VisitReference(type_ctor->layout);

    // TODO(https://fxbug.dev/42143256): Add dependencies introduced through handle constraints.
    // This code currently assumes the handle constraints are always defined in the same
    // library as the resource_definition and so does not check for them separately.
    const auto& invocation = type_ctor->resolved_params;
    if (invocation.size_raw) {
      VisitConstant(invocation.size_raw);
    }
    if (invocation.protocol_decl_raw) {
      VisitConstant(invocation.protocol_decl_raw);
    }
    if (invocation.element_type_raw) {
      VisitReference(invocation.element_type_raw->layout);
    }
    if (invocation.boxed_type_raw) {
      VisitReference(invocation.boxed_type_raw->layout);
    }
  }

  void VisitConstant(const Constant* constant) {
    switch (constant->kind) {
      case Constant::Kind::kLiteral:
        break;
      case Constant::Kind::kIdentifier: {
        auto identifier_constant = static_cast<const IdentifierConstant*>(constant);
        VisitReference(identifier_constant->reference);
        break;
      }
      case Constant::Kind::kBinaryOperator: {
        auto binop_constant = static_cast<const BinaryOperatorConstant*>(constant);
        VisitConstant(binop_constant->left_operand.get());
        VisitConstant(binop_constant->right_operand.get());
        break;
      }
    }
  }

  void VisitReference(const Reference& reference) { deps_.insert(reference.resolved().library()); }
};

}  // namespace

std::unique_ptr<Compilation> Libraries::Filter(const VersionSelection* version_selection) {
  // Returns true if decl should be included based on the version selection.
  auto keep = [&](const auto& decl) {
    return decl->availability.range().Contains(
        version_selection->Lookup(decl->name.library()->platform.value()));
  };

  // Copies decl pointers for which keep() returns true from src to dst.
  auto filter = [&](auto* dst, const auto& src) {
    for (const auto& decl : src) {
      if (keep(decl)) {
        dst->push_back(&*decl);
      }
    }
  };

  // Filters a Library::Declarations into a Compilation::Declarations.
  auto filter_declarations = [&](Compilation::Declarations* dst, const Library::Declarations& src) {
    filter(&dst->bits, src.bits);
    filter(&dst->builtins, src.builtins);
    filter(&dst->consts, src.consts);
    filter(&dst->enums, src.enums);
    filter(&dst->new_types, src.new_types);
    filter(&dst->protocols, src.protocols);
    filter(&dst->resources, src.resources);
    filter(&dst->services, src.services);
    filter(&dst->structs, src.structs);
    filter(&dst->tables, src.tables);
    filter(&dst->aliases, src.aliases);
    filter(&dst->unions, src.unions);
    filter(&dst->overlays, src.overlays);
  };

  ZX_ASSERT(!libraries_.empty());
  auto library = libraries_.back().get();
  auto compilation = std::make_unique<Compilation>();
  compilation->platform = &library->platform.value();
  compilation->version_added = library->availability.set().ranges().first.pair().first;
  compilation->library_name = library->name;
  compilation->library_declarations = library->name_spans;
  compilation->library_attributes = library->attributes.get();
  filter_declarations(&compilation->declarations, library->declarations);
  compilation->external_structs = ExternalStructs(library, compilation->declarations.protocols);
  compilation->using_references = library->dependencies.library_references();
  filter(&compilation->declaration_order, library->declaration_order);
  auto dependencies = CalcDependencies().From(compilation->declaration_order);
  dependencies.erase(library);
  dependencies.erase(root_library());
  for (auto dep_library : dependencies) {
    auto& dep = compilation->direct_and_composed_dependencies.emplace_back();
    dep.library = dep_library;
    filter_declarations(&dep.declarations, dep_library->declarations);
  }
  compilation->version_selection_ = version_selection;

  return compilation;
}

}  // namespace fidlc
