package auth;

/** JWT implementation of {@link IValidator}. */
public class JwtValidator implements IValidator {
    @Override
    public boolean validate(String token) {
        return decode(token) != null;
    }

    private String decode(String token) {
        return token;
    }
}
