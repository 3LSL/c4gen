#include "session.h"

#include "token.h"

namespace app {

void Session::login(const std::string& user) {
    repo_.save(sign(user));
}

}  // namespace app
