// Relationships:
// - `external:sessions-db`: Reads and writes session rows

#pragma once

#include <string>

namespace app {

class Repo {
public:
    void save(const std::string& token);
};

}  // namespace app
