// Relationships:
// None.

#pragma once

#include <string>

namespace app {

inline std::string sign(const std::string& user) {
    return "signed:" + user;
}

}  // namespace app
