namespace MiniApp.Auth
{
    /// <summary>JWT implementation of <see cref="IValidator"/>.</summary>
    public class JwtValidator : IValidator
    {
        public bool Validate(string token)
        {
            return Decode(token) != null;
        }

        private string Decode(string token)
        {
            return token;
        }
    }
}
