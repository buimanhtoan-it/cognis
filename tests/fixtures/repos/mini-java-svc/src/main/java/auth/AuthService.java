package auth;

public class AuthService {
    private final IValidator validator;

    public AuthService(IValidator validator) {
        this.validator = validator;
    }

    public boolean login(String token) {
        return validator.validate(token);
    }
}
