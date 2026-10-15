#pragma once

// Relationships:
// - `include/app/repo.h`: Stores and retrieves session records

#include <string>

#include "app/repo.h"

namespace app {

class Session {
public:
    void login(const std::string& user);

private:
    Repo repo_;
};

}  // namespace app
