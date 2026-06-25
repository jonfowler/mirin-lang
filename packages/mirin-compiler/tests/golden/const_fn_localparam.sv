module widen #(parameter int N) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int w = sum_to(N);
    logic [w-1:0] tmp;
    assign tmp = (w'(x));
    assign result = (8'(tmp));
endmodule
