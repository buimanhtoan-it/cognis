namespace MiniApp.Auth
{
    public class AuthService
    {
        private readonly IValidator _validator;

        public AuthService(IValidator validator)
        {
            _validator = validator;
        }

        public bool Login(string token)
        {
            return _validator.Validate(token);
        }
    }
}
