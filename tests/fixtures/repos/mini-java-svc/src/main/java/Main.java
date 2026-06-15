import auth.AuthService;
import auth.JwtValidator;

public class Main {
    public static void main(String[] args) {
        AuthService service = new AuthService(new JwtValidator());
        service.login("token");
    }
}
